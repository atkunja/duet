import { describe, expect, it } from "vitest";
import { duration, shortId, titleCase } from "./format";

describe("run formatting",()=>{
  it("formats stable compact identifiers",()=>expect(shortId("ab12cd34-0000")).toBe("AB12CD34"));
  it("formats workflow stage names",()=>expect(titleCase("repair-round")).toBe("Repair Round"));
  it("formats durations without decimals",()=>{expect(duration(12_400)).toBe("12s");expect(duration(125_000)).toBe("2m 5s")});
});
