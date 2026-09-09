import { expect, parseColor, test } from "./support";

/** SH-615: CSS Color 4 serializes sRGB channels in the normalized 0–1
 * range, unlike legacy rgb()/rgba()'s 0–255 channels. Contrast probes must
 * give both serializations the same internal units before scoring them. */
test("parseColor normalizes CSS Color 4 sRGB values", () => {
  expect(parseColor("color(srgb 0.25 0.5 0.75)")).toEqual({
    r: 63.75,
    g: 127.5,
    b: 191.25,
    a: 1,
  });
  expect(parseColor("color(srgb 1 0.5 0 / 0.4)")).toEqual({
    r: 255,
    g: 127.5,
    b: 0,
    a: 0.4,
  });
  expect(parseColor("rgba(10, 20, 30, 0.6)")).toEqual({
    r: 10,
    g: 20,
    b: 30,
    a: 0.6,
  });
});
