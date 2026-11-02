//! What the system tells about the computer, on Windows: sleep and wake,
//! session lock, power source, network changes. Elsewhere nothing comes, and
//! the timers of the app keep watching on their own.

use tokio::sync::mpsc::UnboundedSender;

#[cfg_attr(not(windows), allow(dead_code))]
pub enum SystemEvent {
    /// Going to sleep: the system waits (a moment) until `Handled` drops.
    Suspend(Handled),
    /// Back from sleep.
    Resume,
    /// The session was locked (true) or unlocked.
    Locked(bool),
    /// The computer runs on battery (true) or on mains power.
    OnBattery(bool),
    /// An interface or an address changed (one per change: not debounced).
    NetworkChanged,
}

/// Dropped once the event was handled.
#[cfg_attr(not(windows), allow(dead_code))]
pub struct Handled(#[allow(dead_code)] Option<std::sync::mpsc::SyncSender<()>>);

/// Start watching, for the life of the process. True if network changes
/// will be reported.
pub fn watch(events: UnboundedSender<SystemEvent>) -> bool {
    #[cfg(windows)]
    return windows::watch(events);
    #[cfg(not(windows))]
    {
        let _ = events;
        false
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::io;
    use std::ptr::{null, null_mut};
    use std::sync::OnceLock;
    use std::time::Duration;

    use tokio::sync::mpsc::UnboundedSender;
    use windows_sys::core::GUID;
    use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, NO_ERROR, WPARAM};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        NotifyIpInterfaceChange, NotifyUnicastIpAddressChange, MIB_IPINTERFACE_ROW,
        MIB_NOTIFICATION_TYPE, MIB_UNICASTIPADDRESS_ROW,
    };
    use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Power::{
        PowerRegisterSuspendResumeNotification, RegisterPowerSettingNotification,
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, POWERBROADCAST_SETTING,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows_sys::Win32::System::SystemServices::GUID_ACDC_POWER_SOURCE;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        DEVICE_NOTIFY_CALLBACK, DEVICE_NOTIFY_WINDOW_HANDLE, HWND_MESSAGE, MSG,
        PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PBT_POWERSETTINGCHANGE, WM_POWERBROADCAST,
        WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    use super::{Handled, SystemEvent};

    /// How long going to sleep waits for the app.
    const SUSPEND_WAIT: Duration = Duration::from_secs(1);

    static EVENTS: OnceLock<UnboundedSender<SystemEvent>> = OnceLock::new();

    fn send(event: SystemEvent) {
        if let Some(events) = EVENTS.get() {
            let _ = events.send(event);
        }
    }

    fn last_error() -> io::Error {
        io::Error::last_os_error()
    }

    pub fn watch(events: UnboundedSender<SystemEvent>) -> bool {
        if EVENTS.set(events).is_err() {
            return false;
        }
        register_suspend_resume();
        let network = register_network();
        // Session and power source changes come as window messages.
        let started = std::thread::Builder::new()
            .name("system-events".into())
            .spawn(message_loop);
        if let Err(e) = started {
            eprintln!("system events: no message thread: {e}");
        }
        network
    }

    /// Callbacks from a system thread, without any window.
    fn register_suspend_resume() {
        unsafe extern "system" fn on_power(_: *const c_void, kind: u32, _: *const c_void) -> u32 {
            match kind {
                PBT_APMSUSPEND => {
                    let (done, handled) = std::sync::mpsc::sync_channel(1);
                    send(SystemEvent::Suspend(Handled(Some(done))));
                    // Disconnected once handled (or if the app is gone).
                    let _ = handled.recv_timeout(SUSPEND_WAIT);
                }
                PBT_APMRESUMEAUTOMATIC => send(SystemEvent::Resume),
                _ => {}
            }
            NO_ERROR
        }
        // Kept for the life of the process.
        let params = Box::leak(Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(on_power),
            Context: null_mut(),
        }));
        let mut registration = null_mut();
        // SAFETY: the parameters outlive the registration (leaked).
        let error = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                params as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS as HANDLE,
                &mut registration,
            )
        };
        if error != NO_ERROR {
            eprintln!(
                "system events: no sleep notifications: {}",
                io::Error::from_raw_os_error(error as i32)
            );
        }
    }

    /// Interface and address changes, on a system thread.
    fn register_network() -> bool {
        unsafe extern "system" fn on_interface(
            _: *const c_void,
            _: *const MIB_IPINTERFACE_ROW,
            _: MIB_NOTIFICATION_TYPE,
        ) {
            send(SystemEvent::NetworkChanged);
        }
        unsafe extern "system" fn on_address(
            _: *const c_void,
            _: *const MIB_UNICASTIPADDRESS_ROW,
            _: MIB_NOTIFICATION_TYPE,
        ) {
            send(SystemEvent::NetworkChanged);
        }
        // The handles are kept for the life of the process.
        let mut interfaces: HANDLE = null_mut();
        let mut addresses: HANDLE = null_mut();
        // SAFETY: the callbacks take no context and live forever.
        let (a, b) = unsafe {
            (
                NotifyIpInterfaceChange(AF_UNSPEC, Some(on_interface), null(), false, &mut interfaces),
                NotifyUnicastIpAddressChange(AF_UNSPEC, Some(on_address), null(), false, &mut addresses),
            )
        };
        for (what, error) in [("interface", a), ("address", b)] {
            if error != NO_ERROR {
                eprintln!(
                    "system events: no {what} notifications: {}",
                    io::Error::from_raw_os_error(error as i32)
                );
            }
        }
        a == NO_ERROR && b == NO_ERROR
    }

    fn same_guid(a: &GUID, b: &GUID) -> bool {
        (a.data1, a.data2, a.data3, a.data4) == (b.data1, b.data2, b.data3, b.data4)
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_WTSSESSION_CHANGE => match wparam as u32 {
                WTS_SESSION_LOCK => send(SystemEvent::Locked(true)),
                WTS_SESSION_UNLOCK => send(SystemEvent::Locked(false)),
                _ => {}
            },
            WM_POWERBROADCAST if wparam as u32 == PBT_POWERSETTINGCHANGE && lparam != 0 => {
                // SAFETY: the system passes a setting with its data.
                let setting = unsafe { &*(lparam as *const POWERBROADCAST_SETTING) };
                if same_guid(&setting.PowerSetting, &GUID_ACDC_POWER_SOURCE)
                    && setting.DataLength as usize >= size_of::<u32>()
                {
                    // 0: mains, 1: battery, 2: short-term source (UPS).
                    let source =
                        unsafe { std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32) };
                    send(SystemEvent::OnBattery(source != 0));
                }
                return 1;
            }
            _ => {}
        }
        // SAFETY: what the window was given.
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }

    /// A message-only window, never shown, for the notifications that need
    /// one; runs until the process ends.
    fn message_loop() {
        let class: Vec<u16> = "P2PShareSystemEvents\0".encode_utf16().collect();
        // SAFETY: plain Win32 calls; the class name outlives the window.
        unsafe {
            let instance = GetModuleHandleW(null());
            let wc = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            if RegisterClassW(&wc) == 0 {
                eprintln!("system events: no window class: {}", last_error());
                return;
            }
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                instance,
                null(),
            );
            if hwnd.is_null() {
                eprintln!("system events: no window: {}", last_error());
                return;
            }
            if WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) == 0 {
                eprintln!("system events: no session notifications: {}", last_error());
            }
            // Also tells the current power source right away.
            let power = RegisterPowerSettingNotification(
                hwnd as HANDLE,
                &GUID_ACDC_POWER_SOURCE,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            );
            if power == 0 {
                eprintln!("system events: no power source notifications: {}", last_error());
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                DispatchMessageW(&msg);
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn registrations_succeed_and_the_power_source_comes() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(watch(tx), "network notifications");
        // The power source is told right away, through the window.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match rx.try_recv() {
                Ok(SystemEvent::OnBattery(_)) => break,
                Ok(_) => {}
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                Err(e) => panic!("no power source: {e}"),
            }
        }
    }
}
