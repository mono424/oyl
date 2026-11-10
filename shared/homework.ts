/**
 * An exercise sheet split into the parts a student answers. Reads the sheet's
 * Markdown (a PDF's text layer with its `<!-- page N -->` markers, or a
 * converted LaTeX sheet) and finds its structure: questions ("Question 2",
 * "Problem 3", "### 1. Title", or on a sheet without those a plain "1. Title"
 * line), their lettered parts ("Part (b)", "(c)", "a)") and roman sub-parts
 * ("(ii)"). Each deepest part becomes one answer; the
 * intro text above it travels along as context. Deterministic and free of
 * I/O, so the app runs it in the browser and tests run it in Node.
 */

export interface SheetPart {
  /** Stable within the sheet: "1.b.ii", "2.c", "4". */
  key: string;
  /** The question's number, "1". */
  question: string;
  questionTitle: string | null;
  /** The part within its question, as the margin shows it: "b (ii)", "(c)", "" for a whole question. */
  label: string;
  points: number | null;
  /** The sheet page the part starts on, for PDF sheets. */
  page: number | null;
  /** The part's own text. */
  prompt: string;
  /** Text above it that it builds on: the question's and part's introductions. */
  context: string | null;
  /**
   * Where the part's excerpt starts on the PDF page: the label of the part,
   * or of its question or lettered part when it is their first.
   */
  anchor: string;
  /** The page `anchor` is on. */
  anchorPage: number | null;
}

interface Node {
  level: 1 | 2 | 3;
  id: string;
  title: string | null;
  points: number | null;
  page: number | null;
  /** The label as printed, to find it on the page: "Question 1", "Part (b)", "(ii)". */
  anchor: string;
  lines: string[];
  children: Node[];
  parent: Node | null;
}

const QUESTION = /^(?:question|problem|exercise|aufgabe|task|frage)\s*(\d+)\s*[.:)\-]?\s*(.*)$/i;
const HEADING_NUMBER = /^#{1,6}\s+(?:\*\*)?(\d+)[.)](?:\*\*)?\s+(.*)$/;
// A PDF's text layer loses heading styles: "1. Two Versions of CPA security (10 points)", "2 Randomized MACs".
const PLAIN_HEADING = /^(\d{1,2})(\.?)\s+(\S.*)$/;
// A numbered list item inside a question: "3. Computation:".
const NUMBERED_ITEM = /^\d{1,2}[.)]\s+\S/;
const LETTER = /^(?:part\s*\(?([a-z])\)?|\(([a-z])\)|([a-z])\))\s*[.:,]?\s*(.*)$/i;
const ROMAN = /^(?:\(([ivx]{1,5})\)|([ivx]{1,5})\))\s*[.:,]?\s*(.*)$/i;
const QUESTION_LABEL = /^((?:question|problem|exercise|aufgabe|task|frage)\s*\d+)/i;
const LETTER_LABEL = /^(part\s*\(?[a-z]\)?|\([a-z]\)|[a-z]\))/i;
const ROMAN_LABEL = /^(\([ivx]{1,5}\)|[ivx]{1,5}\))/i;
const POINTS = /(\d+(?:\.\d+)?)\s*(?:points?|pts?\.?|marks?|punkte?|p\.)(?![a-z])/i;
// Where a sheet's questions end: a formula sheet or appendix after the last one.
const BACK_MATTER = /^(?:#{1,6}\s+)?(?:constants|useful (?:constants|relationships|formulas)|formula sheet|appendix|reference(?:s| sheet)?)\b/i;

const ROMANS = ['i', 'ii', 'iii', 'iv', 'v', 'vi', 'vii', 'viii', 'ix', 'x', 'xi', 'xii'];

/** A line without Markdown list, quote and emphasis markers. */
const bare = (line: string) =>
  line
    .trim()
    .replace(/^>\s*/, '')
    .replace(/^[-*+]\s+/, '')
    .replace(/^\*\*([^*]+)\*\*/, '$1')
    .replace(/^_([^_]+)_/, '$1')
    .trim();

/**
 * A plain line that reads as the heading of question `n`: short, not a
 * sentence or a list item that runs on ("2. The adversary is given ...",
 * "3. Computation:"), and without a dot only when it starts like a title.
 */
const plainHeading = (line: string, n: number) => {
  const m = PLAIN_HEADING.exec(line);
  if (!m || Number(m[1]) !== n) return null;
  const title = titleOf(m[3]) ?? '';
  if (!title || title.length > 70 || title.split(/\s+/).length > 8 || /[.,;:]$/.test(m[3].trim())) return null;
  if (!m[2] && !/^[A-Z(]/.test(m[3])) return null;
  return { id: m[1], rest: m[3], anchor: `${m[1]}${m[2]} ${m[3].split(/\s+/)[0]}` };
};

const pointsOf = (text: string) => {
  const m = POINTS.exec(text);
  return m ? Number(m[1]) : null;
};

/** A title without its points: "Molecular absorption (20 points)" -> "Molecular absorption". */
const titleOf = (text: string) =>
  text
    .replace(/\(?\s*\d+(?:\.\d+)?\s*(?:points?|pts?\.?|marks?|punkte?)\s*\)?/i, '')
    .replace(/[\s.:,\-]+$/, '')
    .trim() || null;

export function parseSheet(markdown: string): SheetPart[] {
  const root: Node = { level: 1, id: '', title: null, points: null, page: null, anchor: '', lines: [], children: [], parent: null };
  let question: Node | null = null;
  let letter: Node | null = null;
  let current: Node = root;
  let page: number | null = null;
  let ended = false;
  // Only a sheet without labelled or Markdown questions has plain headings.
  const lines = (markdown ?? '').split('\n');
  const plain = !lines.some((raw) => QUESTION.test(bare(raw)) || HEADING_NUMBER.test(raw.trim()));
  // Under a plain heading, lettered lines after a numbered item are that item's list, not parts.
  let listed = false;

  const open = (level: 1 | 2 | 3, id: string, rest: string, anchor: string, parent: Node): Node => {
    const node: Node = { level, id, title: level === 1 ? titleOf(rest) : null, points: pointsOf(rest), page, anchor, lines: [], children: [], parent };
    // The label line's text after the label is the start of the part's own text.
    if (level > 1 && rest.trim()) node.lines.push(rest.trim());
    parent.children.push(node);
    return node;
  };

  for (const raw of lines) {
    const marker = /^<!--\s*page\s+(\d+)\s*-->$/.exec(raw.trim());
    if (marker) {
      page = Number(marker[1]);
      continue;
    }
    if (/^<!--[\s\S]*-->$/.test(raw.trim())) continue;
    const line = bare(raw);
    if (ended) continue;
    if (question && BACK_MATTER.test(raw.trim())) {
      ended = true;
      continue;
    }

    const q = QUESTION.exec(line);
    const h = q ? null : HEADING_NUMBER.exec(raw.trim());
    // Questions count up. A bare "Question 4." is a wrapped line of running text
    // ("include diagrams for / Question 4.") unless it is the next number.
    const n = q || h ? Number((q ?? h)![1]) : NaN;
    const last = question ? Number(question.id) : 0;
    const isQuestion = (q || h) && n > last && (((q ?? h)![2] ?? '').trim() !== '' || n === last + 1);
    if (isQuestion) {
      const id = (q ?? h)![1];
      question = open(1, id, (q ?? h)![2] ?? '', q ? QUESTION_LABEL.exec(line)![1] : `${id}.`, root);
      letter = null;
      current = question;
      continue;
    }
    const ph = plain ? plainHeading(line, (question ? Number(question.id) : 0) + 1) : null;
    if (ph) {
      question = open(1, ph.id, ph.rest, ph.anchor, root);
      letter = null;
      current = question;
      listed = false;
      continue;
    }
    if (plain && question && NUMBERED_ITEM.test(line)) listed = true;
    if (listed) {
      current.lines.push(raw.trimEnd());
      continue;
    }

    const r = ROMAN.exec(line);
    const l = LETTER.exec(line);
    const romanId = r ? (r[1] ?? r[2]).toLowerCase() : null;
    // "(i)" is a letter only right after "(h)" in the same question; otherwise a roman sub-part.
    const lastLetter = question?.children.filter((c) => c.level === 2).at(-1)?.id ?? null;
    const nextLetter = lastLetter ? String.fromCharCode(lastLetter.charCodeAt(0) + 1) : null;
    if (r && romanId && ROMANS.includes(romanId) && romanId !== nextLetter) {
      current = open(3, romanId, r[3] ?? '', ROMAN_LABEL.exec(line)![1], letter ?? question ?? root);
      continue;
    }
    if (l) {
      const id = (l[1] ?? l[2] ?? l[3]).toLowerCase();
      letter = open(2, id, l[4] ?? '', LETTER_LABEL.exec(line)![1], question ?? root);
      current = letter;
      continue;
    }
    current.lines.push(raw.trimEnd());
  }

  const text = (n: Node) => n.lines.join('\n').replace(/\n{3,}/g, '\n\n').trim();
  const parts: SheetPart[] = [];
  const visit = (n: Node) => {
    if (n.children.length) {
      n.children.forEach(visit);
      return;
    }
    if (n === root) return;
    const chain: Node[] = [];
    for (let p: Node | null = n; p && p !== root; p = p.parent) chain.unshift(p);
    const q = chain.find((c) => c.level === 1) ?? null;
    const l = chain.find((c) => c.level === 2) ?? null;
    const r = chain.find((c) => c.level === 3) ?? null;
    // The excerpt starts at the highest ancestor this part opens.
    let start: Node = n;
    while (start.parent && start.parent !== root && start.parent.children[0] === start) start = start.parent;
    const context = chain
      .slice(0, -1)
      .map((c) => text(c))
      .filter(Boolean)
      .join('\n\n');
    parts.push({
      key: [q?.id ?? '1', l?.id, r?.id].filter(Boolean).join('.'),
      question: q?.id ?? '1',
      questionTitle: q?.title ?? null,
      label: [l ? l.id : null, r ? `(${r.id})` : null].filter(Boolean).join(' ') || (q ? '' : 'Answer'),
      points: n.points,
      page: n.page,
      prompt: text(n),
      context: context || null,
      anchor: start.anchor,
      anchorPage: start.page,
    });
  };
  visit(root);

  // No structure found: the whole sheet is one answer.
  if (!parts.length) {
    const all = text(root);
    return [{ key: '1', question: '1', questionTitle: null, label: 'Answer', points: null, page: 1, prompt: all, context: null, anchor: '', anchorPage: 1 }];
  }
  // Keys stay unique if a sheet repeats a label.
  const seen = new Map<string, number>();
  for (const p of parts) {
    const n = seen.get(p.key) ?? 0;
    seen.set(p.key, n + 1);
    if (n) p.key = `${p.key}~${n + 1}`;
  }
  return parts;
}
