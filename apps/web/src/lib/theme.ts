import { createSignal } from 'solid-js';

/**
 * Light and dark, as in the desktop app (crates/oyl-ui/src/theme.rs):
 * `system` follows the device, the toggle makes an explicit choice. The
 * resolved look is `data-theme` on <html>, which global.css reads; index.html
 * sets it before the first paint from the same stored choice.
 */
export type Appearance = 'system' | 'light' | 'dark';

const KEY = 'oyl:appearance';
const media = matchMedia('(prefers-color-scheme: dark)');

/** The browser chrome's color: Oyl's veil, light and dark. */
const CHROME = { light: '#f3f1ec', dark: '#15151a' } as const;

function stored(): Appearance {
  try {
    const value = localStorage.getItem(KEY);
    return value === 'light' || value === 'dark' ? value : 'system';
  } catch {
    return 'system';
  }
}

const [appearance, setChoice] = createSignal<Appearance>(stored());
const [systemDark, setSystemDark] = createSignal(media.matches);

/** The user's choice: system, light or dark. */
export { appearance };

/** Whether dark is on screen now. */
export const isDark = () => appearance() === 'dark' || (appearance() === 'system' && systemDark());

function apply() {
  const dark = isDark();
  document.documentElement.dataset.theme = dark ? 'dark' : 'light';
  for (const meta of document.querySelectorAll<HTMLMetaElement>('meta[name="theme-color"]')) {
    meta.content = dark ? CHROME.dark : CHROME.light;
  }
}

export function setAppearance(next: Appearance) {
  setChoice(next);
  try {
    if (next === 'system') localStorage.removeItem(KEY);
    else localStorage.setItem(KEY, next);
  } catch {
    // Private mode: the choice lasts for this page only.
  }
  apply();
}

/** Flips what is on screen right now, and so leaves `system` for an explicit choice. */
export function toggleAppearance() {
  setAppearance(isDark() ? 'light' : 'dark');
}

media.addEventListener('change', (event) => {
  setSystemDark(event.matches);
  apply();
});
apply();
