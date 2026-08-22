import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../lib/api";
import { Settings } from "./Settings";

vi.mock("../lib/api", () => ({
  api: {
    doctor: vi.fn(),
    getPreferences: vi.fn(),
    savePreferences: vi.fn(),
  },
  errorMessage: (error: unknown) => String(error),
}));

describe("Settings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.getPreferences).mockResolvedValue({ editor: "auto", maxRepairs: 3 });
  });

  it("announces asynchronous diagnostic failures", async () => {
    vi.mocked(api.doctor).mockRejectedValue(new Error("diagnostics unavailable"));

    render(<Settings onBack={vi.fn()} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("diagnostics unavailable");
  });
});
