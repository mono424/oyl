/** How a study session went, as ChatGPT logged it (`call.mode`). */
const MODE_LABELS: Record<string, string> = { explain: 'Explain it', quiz: 'Quiz me', qa: 'I ask' };

export const modeLabel = (mode: string | null | undefined) => MODE_LABELS[mode ?? ''] ?? '';

/** A session the student held in an assistant (call.assistant) is marked with its name in lists. */
export const viaLabel = (assistant: string | null | undefined) => (assistant ? ` · ${assistant}` : '');
