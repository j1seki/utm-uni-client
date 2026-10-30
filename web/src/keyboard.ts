// Keyboard helpers shared by the display client.

/** SPICE modifier bits (see src/spice/keymap.rs). */
export function modifierMask(pressed: ReadonlySet<string>): number {
  let mask = 0;
  if (pressed.has("ShiftLeft")) mask |= 1 << 0;
  if (pressed.has("ShiftRight")) mask |= 1 << 1;
  if (pressed.has("ControlLeft")) mask |= 1 << 2;
  if (pressed.has("ControlRight")) mask |= 1 << 3;
  if (pressed.has("AltLeft")) mask |= 1 << 4;
  if (pressed.has("AltRight")) mask |= 1 << 5;
  if (pressed.has("MetaLeft") || pressed.has("OSLeft")) mask |= 1 << 6;
  if (pressed.has("MetaRight") || pressed.has("OSRight")) mask |= 1 << 7;
  return mask;
}

const MODIFIER_CODES = new Set([
  "ShiftLeft",
  "ShiftRight",
  "ControlLeft",
  "ControlRight",
  "AltLeft",
  "AltRight",
  "MetaLeft",
  "MetaRight",
  "OSLeft",
  "OSRight",
]);

export function isModifier(code: string): boolean {
  return MODIFIER_CODES.has(code);
}

/** Codes handled by the backend scancode map. */
const MAPPED = new Set([
  "Escape", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6",
  "Digit7", "Digit8", "Digit9", "Digit0", "Minus", "Equal", "Backspace",
  "Tab", "KeyQ", "KeyW", "KeyE", "KeyR", "KeyT", "KeyY", "KeyU", "KeyI",
  "KeyO", "KeyP", "BracketLeft", "BracketRight", "Enter", "ControlLeft",
  "KeyA", "KeyS", "KeyD", "KeyF", "KeyG", "KeyH", "KeyJ", "KeyK", "KeyL",
  "Semicolon", "Quote", "Backquote", "ShiftLeft", "Backslash", "KeyZ",
  "KeyX", "KeyC", "KeyV", "KeyB", "KeyN", "KeyM", "Comma", "Period",
  "Slash", "ShiftRight", "NumpadMultiply", "AltLeft", "Space", "CapsLock",
  "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "NumLock",
  "ScrollLock", "Numpad7", "Numpad8", "Numpad9", "NumpadSubtract",
  "Numpad4", "Numpad5", "Numpad6", "NumpadAdd", "Numpad1", "Numpad2",
  "Numpad3", "Numpad0", "NumpadDecimal", "IntlBackslash", "F11", "F12",
  "NumpadEnter", "ControlRight", "NumpadDivide", "PrintScreen", "AltRight",
  "Home", "ArrowUp", "PageUp", "ArrowLeft", "ArrowRight", "End",
  "ArrowDown", "PageDown", "Insert", "Delete", "MetaLeft", "OSLeft",
  "MetaRight", "OSRight", "ContextMenu",
]);

export function isMapped(code: string): boolean {
  return MAPPED.has(code);
}
