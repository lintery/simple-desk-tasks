// Thin wrapper over the Tauri globals (withGlobalTauri: true).

const T = window.__TAURI__;
const me = T.webviewWindow.getCurrentWebviewWindow();

export const label = me.label;
/** Brings this widget to keyboard focus (e.g. so Ctrl+Z reaches its snackbar). */
export const focusSelf = () => me.setFocus().catch(() => {});
export const invoke = (cmd, args) => T.core.invoke(cmd, args);
/** Events broadcast to every window (`app.emit`). */
export const listenAll = (event, cb) => T.event.listen(event, (e) => cb(e.payload));
/** Events addressed to this window only (`emit_to(label, …)`). */
export const listenMine = (event, cb) => me.listen(event, (e) => cb(e.payload));

export const api = {
  tasks: () => invoke('get_tasks'),
  add: (t) => invoke('add_task', { title: t.title, due: t.due ?? null, time: t.time ?? null, repeat: t.repeat ?? null }),
  update: (id, patch) => invoke('update_task', { id, patch }),
  remove: (id) => invoke('delete_task', { id }),
  /** Puts a deleted task back exactly as it was (same id, age, reminder state). */
  restore: (task) => invoke('restore_task', { task }),
  backdrop: () => invoke('get_backdrop'),
  /** null when fine, otherwise why tasks.json can't be read. */
  storeStatus: () => invoke('store_status'),
  hotkey: () => invoke('hotkey_label'),
  movable: () => invoke('widgets_movable'),
  openUrl: (url) => invoke('open_url', { url }),
  /** Tells every widget which task is being dragged (null when the drag ends). */
  taskDrag: (id, token) => invoke('task_drag', { id, token }),
  /** No window took the drop: let the widget under the mouse handle it. */
  dropAtCursor: (id) => invoke('drop_at_cursor', { from: label, id }),
  hideQuick: () => invoke('hide_quick'),
  focus: () => invoke('focus_state'),
  /** 'start' | 'pause' | 'skip' | 'reset'; resolves to the new timer state. */
  focusAction: (action) => invoke('focus_action', { action }),
  focusMode: (mode) => invoke('focus_mode', { mode }),
  /** Remembers the focus sound (null = silence) and its volume (0..1). */
  focusPrefs: (sound, volume) => invoke('focus_prefs', { sound, volume }),
  /** What the player widget shows, or null. */
  media: () => invoke('media_state'),
  /** 'toggle' | 'next' | 'prev'; 'seek' to `value` ms; 'volume' to `value` 0..1; 'mute' 1 / 0. */
  mediaControl: (action, value = null) => invoke('media_control', { action, value }),
};
