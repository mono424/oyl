/** Divider-tab colors a course can carry. Keys are stored on `course.color`. */
export const COURSE_COLORS = {
  blue: '#8fb8e8',
  orange: '#f2a97e',
  green: '#9ed3a8',
  lilac: '#cfa6e0',
  yellow: '#f0cd5e',
  red: '#e89a9a',
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
