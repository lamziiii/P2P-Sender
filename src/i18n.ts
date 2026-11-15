// Interface language: French, or English for any other system language.
// The settings may choose one; the choice is kept here (read before the
// first render) and told to the backend, which writes its own texts in it
// (notifications, tray menu, errors). The window reloads on a change.

type Lang = 'fr' | 'en';
// "" follows the system.
export type LangChoice = Lang | '';

const CHOICE_KEY = 'language';

const readChoice = (): LangChoice => {
  try {
    const v = localStorage.getItem(CHOICE_KEY);
    return v === 'fr' || v === 'en' ? v : '';
  } catch {
    return '';
  }
};

export const systemLang: Lang = /^fr\b/i.test(navigator.language || '') ? 'fr' : 'en';
export const langChoice: LangChoice = readChoice();
export const lang: Lang = langChoice || systemLang;
export const isEnglish = lang === 'en';
// For dates and numbers.
export const locale = isEnglish ? 'en-GB' : 'fr-FR';

document.documentElement.lang = lang;

// The same text in both languages, picked once for the whole session.
export const tr = <T,>(fr: T, en: T): T => (isEnglish ? en : fr);

// Saved for the next load (the caller reloads the window).
export const saveLangChoice = (choice: LangChoice) => {
  try {
    if (choice) localStorage.setItem(CHOICE_KEY, choice);
    else localStorage.removeItem(CHOICE_KEY);
  } catch {
    // Not kept: the system's language stays.
  }
};
