export const formatBytes = (n: number) => {
  if (n >= 1024 ** 3) return `${(n / 1024 ** 3).toFixed(2)} Go`;
  if (n >= 1024 ** 2) return `${(n / 1024 ** 2).toFixed(1)} Mo`;
  if (n >= 1024) return `${(n / 1024).toFixed(0)} Ko`;
  return `${n} o`;
};

export const formatTime = (ts: number) =>
  new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });

export const formatDate = (ts: number) => {
  const d = new Date(ts);
  const today = new Date();
  if (d.toDateString() === today.toDateString()) return formatTime(ts);
  return d.toLocaleDateString([], { day: 'numeric', month: 'short' });
};
