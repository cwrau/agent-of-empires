// Branch coverage for the client-side cron validator (#2886). Each field has a
// distinct range, and the validator supports wildcards, lists, ranges, and
// steps; every one of those forms has an accept and a reject case here.

import { describe, expect, it } from "vitest";
import { validateCron } from "../cronValidation";

describe("validateCron field count", () => {
  it("rejects too few fields", () => {
    expect(validateCron("0 8 * *")).toMatch(/exactly 5 fields/);
  });

  it("rejects too many fields", () => {
    expect(validateCron("0 8 * * * *")).toMatch(/exactly 5 fields/);
  });

  it("accepts the all-wildcard expression", () => {
    expect(validateCron("* * * * *")).toBeNull();
  });
});

describe("validateCron per-field ranges", () => {
  // [expr, valid?, fieldName]
  const cases: [string, boolean, string][] = [
    // minute 0-59
    ["0 * * * *", true, "minute low"],
    ["59 * * * *", true, "minute high"],
    ["60 * * * *", false, "minute over"],
    // hour 0-23
    ["0 0 * * *", true, "hour low"],
    ["0 23 * * *", true, "hour high"],
    ["0 24 * * *", false, "hour over"],
    // day-of-month 1-31
    ["0 0 1 * *", true, "dom low"],
    ["0 0 31 * *", true, "dom high"],
    ["0 0 0 * *", false, "dom under"],
    ["0 0 32 * *", false, "dom over"],
    // month 1-12
    ["0 0 1 1 *", true, "month low"],
    ["0 0 1 12 *", true, "month high"],
    ["0 0 1 0 *", false, "month under"],
    ["0 0 1 13 *", false, "month over"],
    // day-of-week 0-7 (0 and 7 both Sunday)
    ["0 0 * * 0", true, "dow low"],
    ["0 0 * * 7", true, "dow high"],
    ["0 0 * * 8", false, "dow over"],
  ];

  it.each(cases)("%s -> %s (%s)", (expr, valid) => {
    const result = validateCron(expr);
    if (valid) expect(result).toBeNull();
    else expect(result).not.toBeNull();
  });
});

describe("validateCron token forms", () => {
  it("accepts comma lists", () => {
    expect(validateCron("0,15,30,45 * * * *")).toBeNull();
  });

  it("rejects a list with an out-of-range member", () => {
    expect(validateCron("0,15,99 * * * *")).toMatch(/minute/);
  });

  it("accepts ranges", () => {
    expect(validateCron("0 9-17 * * *")).toBeNull();
  });

  it("rejects a range whose endpoint is out of range", () => {
    expect(validateCron("0 9-25 * * *")).toMatch(/hour/);
  });

  it("rejects a malformed 3-part range", () => {
    expect(validateCron("0 1-2-3 * * *")).toMatch(/hour/);
  });

  it("accepts step values", () => {
    expect(validateCron("*/15 * * * *")).toBeNull();
    expect(validateCron("0 */2 * * *")).toBeNull();
  });

  it("rejects a zero step", () => {
    expect(validateCron("*/0 * * * *")).toMatch(/minute/);
  });

  it("rejects a non-numeric step", () => {
    expect(validateCron("*/x * * * *")).toMatch(/minute/);
  });

  it("rejects more than one slash", () => {
    expect(validateCron("*/2/3 * * * *")).toMatch(/minute/);
  });

  it("rejects non-numeric tokens", () => {
    expect(validateCron("abc * * * *")).toMatch(/minute/);
    expect(validateCron("0 xyz * * *")).toMatch(/hour/);
  });
});
