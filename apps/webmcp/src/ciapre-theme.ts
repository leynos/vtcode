import { tags } from "@lezer/highlight";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorState, type Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";

export const CIAPRE_COLOURS = Object.freeze({
  background: "#181818",
  foreground: "#aea47f",
  primary: "#aea47f",
  secondary: "#cc8a3e",
  alert: "#c16a68",
  surface: "#202020",
  surfaceRaised: "#262626",
  border: "#48402f",
  muted: "#8f896b",
  selection: "#4a3b25",
  added: "#b9c98a",
  removed: "#c16a68",
  alertForeground: "#d98582",
  keyword: "#e0b86a",
  string: "#b9c98a",
  number: "#d7b56d",
  comment: "#aaa27f",
  function: "#d8a262",
  type: "#c5a6d8",
  variable: "#d0c6a0",
});

export const ciapreHighlightStyle = HighlightStyle.define([
  { tag: tags.comment, color: CIAPRE_COLOURS.comment, fontStyle: "italic" },
  { tag: [tags.keyword, tags.operatorKeyword, tags.controlKeyword], color: CIAPRE_COLOURS.keyword },
  { tag: [tags.string, tags.special(tags.string)], color: CIAPRE_COLOURS.string },
  { tag: [tags.number, tags.bool, tags.null], color: CIAPRE_COLOURS.number },
  { tag: [tags.function(tags.variableName), tags.labelName], color: CIAPRE_COLOURS.function },
  { tag: [tags.typeName, tags.className, tags.namespace], color: CIAPRE_COLOURS.type },
  { tag: [tags.variableName, tags.propertyName], color: CIAPRE_COLOURS.variable },
  { tag: tags.definition(tags.variableName), color: CIAPRE_COLOURS.primary },
  { tag: tags.meta, color: CIAPRE_COLOURS.secondary },
]);

export const ciapreTheme = EditorView.theme({
  "&": { color: CIAPRE_COLOURS.foreground, backgroundColor: CIAPRE_COLOURS.background, height: "100%" },
  ".cm-scroller": { overflow: "auto", fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace", fontSize: "13px" },
  ".cm-content": { caretColor: CIAPRE_COLOURS.secondary, padding: "16px 0 24px" },
  ".cm-line": { padding: "0 18px" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: CIAPRE_COLOURS.secondary, borderLeftWidth: "2px" },
  ".cm-gutters": { backgroundColor: CIAPRE_COLOURS.surface, color: CIAPRE_COLOURS.muted, border: "0", borderRight: `1px solid ${CIAPRE_COLOURS.border}` },
  ".cm-gutterElement": { padding: "0 10px 0 8px" },
  ".cm-activeLine": { backgroundColor: "#2a261f" },
  ".cm-activeLineGutter": { backgroundColor: "#2a261f", color: CIAPRE_COLOURS.foreground },
  ".cm-selectionBackground, ::selection": { backgroundColor: `${CIAPRE_COLOURS.selection} !important` },
  ".cm-focused .cm-selectionBackground": { backgroundColor: `${CIAPRE_COLOURS.selection} !important` },
  ".cm-tooltip": { backgroundColor: CIAPRE_COLOURS.surfaceRaised, color: CIAPRE_COLOURS.foreground, border: `1px solid ${CIAPRE_COLOURS.border}` },
  ".cm-panels": { backgroundColor: CIAPRE_COLOURS.surfaceRaised, color: CIAPRE_COLOURS.foreground },
});

export const editorDefaults: Extension[] = [
  EditorState.tabSize.of(4),
  EditorView.lineWrapping,
  ciapreTheme,
  syntaxHighlighting(ciapreHighlightStyle, { fallback: true }),
];
