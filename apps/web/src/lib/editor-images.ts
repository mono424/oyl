import { EditorSelection, StateEffect, StateField } from '@codemirror/state';
import { Decoration, EditorView, WidgetType } from '@codemirror/view';
import { isImage } from './images';

/**
 * Images coming into the CodeMirror editors (components/Editor.tsx and
 * TexDocumentEditor.tsx): where each goes once uploaded, kept in place while
 * you type on, with a marker there until it has.
 */

interface Upload {
  key: number;
  pos: number;
}

/** Places an upload, moves it on, or (`pos: null`) ends it. */
export const setUpload = StateEffect.define<{ key: number; pos: number | null }>();

export const uploadsField = StateField.define<readonly Upload[]>({
  create: () => [],
  update(list, tr) {
    let next = tr.docChanged ? list.map((u) => ({ key: u.key, pos: tr.changes.mapPos(u.pos, -1) })) : list;
    for (const e of tr.effects) {
      if (!e.is(setUpload)) continue;
      const { key, pos } = e.value;
      next = next.filter((u) => u.key !== key);
      if (pos !== null) next = [...next, { key, pos }];
    }
    return next;
  },
});

class UploadingWidget extends WidgetType {
  eq() {
    return true;
  }
  toDOM() {
    const el = document.createElement('span');
    el.className = 'cm-hw-uploading';
    el.setAttribute('role', 'status');
    el.textContent = 'Adding the image…';
    return el;
  }
}

export const uploading = Decoration.widget({ widget: new UploadingWidget(), side: -1 });

export const isFileDrag = (e: DragEvent) => !!e.dataTransfer?.types.includes('Files');

export const imagesOf = (data: DataTransfer | null) => [...(data?.files ?? [])].filter(isImage);

let uploads = 0;

/** Uploads images one by one, each written in where the text it came to now stands. */
export function addImagesAt(view: EditorView, pos: number, files: File[], upload: (file: File) => Promise<string | null>, alive: () => boolean) {
  const key = ++uploads;
  view.dispatch({ effects: setUpload.of({ key, pos }) });
  void (async () => {
    for (const file of files) {
      const written = await upload(file);
      const at = alive() ? view.state.field(uploadsField).find((u) => u.key === key)?.pos : undefined;
      if (!written || at === undefined) continue;
      const line = view.state.doc.lineAt(at);
      const insert = `${at === line.from ? '' : '\n'}${written}\n`;
      const follow = view.state.selection.main.empty && view.state.selection.main.head === at;
      view.dispatch({
        changes: { from: at, insert },
        selection: follow ? EditorSelection.cursor(at + insert.length) : undefined,
        effects: setUpload.of({ key, pos: at + insert.length }),
        userEvent: 'input.paste',
      });
    }
    if (alive()) view.dispatch({ effects: setUpload.of({ key, pos: null }) });
  })();
}

/** A file dropped beside the editor would open in place of the app. Returns the undo. */
export function guardFileDrops(view: EditorView) {
  const guard = (e: DragEvent) => {
    if (e.defaultPrevented || !isFileDrag(e) || view.contentDOM.contains(e.target as Node)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'none';
  };
  window.addEventListener('dragover', guard);
  window.addEventListener('drop', guard);
  return () => {
    window.removeEventListener('dragover', guard);
    window.removeEventListener('drop', guard);
  };
}
