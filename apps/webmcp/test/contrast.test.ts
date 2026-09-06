import test from "node:test";
import assert from "node:assert/strict";
import { CIAPRE_COLOURS } from "../src/ciapre-theme.ts";

function relativeLuminance(hex: string): number {
  const channels = hex.slice(1).match(/../g)?.map((channel) => Number.parseInt(channel, 16) / 255) || [];
  const linear = channels.map((channel) => channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
  return 0.2126 * (linear[0] ?? 0) + 0.7152 * (linear[1] ?? 0) + 0.0722 * (linear[2] ?? 0);
}

function contrast(foreground: string, background: string): number {
  const light = relativeLuminance(foreground);
  const dark = relativeLuminance(background);
  return (Math.max(light, dark) + 0.05) / (Math.min(light, dark) + 0.05);
}

test("Ciapre text, controls, syntax, and status colours meet AA on their actual dark surfaces", () => {
  const normalText: readonly string[] = [CIAPRE_COLOURS.foreground, CIAPRE_COLOURS.primary, CIAPRE_COLOURS.secondary, CIAPRE_COLOURS.muted, CIAPRE_COLOURS.added, CIAPRE_COLOURS.alertForeground];
  for (const foreground of normalText) {
    assert.ok(contrast(foreground, CIAPRE_COLOURS.background) >= 4.5, `${foreground} on background`);
    assert.ok(contrast(foreground, CIAPRE_COLOURS.surface) >= 4.5, `${foreground} on surface`);
  }
  for (const syntax of [CIAPRE_COLOURS.keyword, CIAPRE_COLOURS.string, CIAPRE_COLOURS.number, CIAPRE_COLOURS.comment, CIAPRE_COLOURS.function, CIAPRE_COLOURS.type, CIAPRE_COLOURS.variable]) {
    assert.ok(contrast(syntax, CIAPRE_COLOURS.background) >= 4.5, `${syntax} syntax token`);
  }
  assert.ok(contrast(CIAPRE_COLOURS.alert, CIAPRE_COLOURS.background) >= 4.5, "source alert on application background");

  const controlPairs: readonly (readonly [string, string, string])[] = [
    ["primary", CIAPRE_COLOURS.background, CIAPRE_COLOURS.primary],
    ["approve", CIAPRE_COLOURS.background, CIAPRE_COLOURS.secondary],
    ["danger", CIAPRE_COLOURS.alertForeground, CIAPRE_COLOURS.surfaceRaised],
    ["subtle", CIAPRE_COLOURS.foreground, CIAPRE_COLOURS.surfaceRaised],
  ];
  for (const [control, foreground, background] of controlPairs) {
    assert.ok(contrast(foreground, background) >= 4.5, `${control} control`);
  }
});
