import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { StatusBadge, StatusIcon } from "./Status";

describe("workflow status",()=>{
  it("labels completed runs",()=>{render(<StatusBadge status="completed"/>);expect(screen.getByText("completed")).toBeInTheDocument()});
  it("exposes semantic visual states",()=>{const {container}=render(<StatusIcon status="failed"/>);expect(container.querySelector(".danger")).toBeInTheDocument()});
});
