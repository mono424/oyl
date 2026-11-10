/**
 * Divider-tab colors a course can carry. Keys are stored on `course.color`.
 * Quiet, dusty tones that sit beside Oyl's one amber accent without competing
 * with it.
 */
export const COURSE_COLORS = {
  blue: '#9aaec4',
  orange: '#d4a07c',
  green: '#9cb89e',
  lilac: '#b6a3c2',
  yellow: '#d2b56c',
  red: '#cc958f',
} as const;

export type CourseColor = keyof typeof COURSE_COLORS;

export const courseColor = (key: string | null | undefined) =>
  COURSE_COLORS[(key ?? 'blue') as CourseColor] ?? COURSE_COLORS.blue;

export const LANGUAGES: { code: string; label: string }[] = [
  { code: 'en', label: 'English' },
  { code: 'de', label: 'German' },
  { code: 'fr', label: 'French' },
  { code: 'es', label: 'Spanish' },
  { code: 'it', label: 'Italian' },
  { code: 'nl', label: 'Dutch' },
  { code: 'pt', label: 'Portuguese' },
  { code: 'pl', label: 'Polish' },
  { code: 'tr', label: 'Turkish' },
];
