import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "../lib/api";
import { Settings } from "./Settings";

vi.mock("../lib/api", () => ({
  api: {
    doctor: vi.fn(),
    getPreferences: vi.fn(),
    savePreferences: vi.fn(),
    loginCodex: vi.fn(),
    codexAuthInProgress: vi.fn(),
    cancelCodexLogin: vi.fn(),
  },
  errorMessage: (error: unknown) => String(error),
}));

describe("Settings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(api.getPreferences).mockResolvedValue({ editor: "auto", maxRepairs: 3 });
    vi.mocked(api.codexAuthInProgress).mockResolvedValue(false);
  });

  it("announces asynchronous diagnostic failures", async () => {
    vi.mocked(api.doctor).mockRejectedValue(new Error("diagnostics unavailable"));

    render(<Settings onBack={vi.fn()} />);

    expect(await screen.findByRole("alert")).toHaveTextContent("diagnostics unavailable");
  });

  it("starts the official Codex browser sign-in and refreshes its status", async () => {
    vi.mocked(api.doctor).mockResolvedValue({
      appDataWritable: true,
      databaseHealthy: true,
      git: { installed: true, detail: "Available" },
      claude: { installed: true, authenticated: true, detail: "Available" },
      codex: { installed: true, authenticated: false, detail: "Login required" },
      os: "macOS",
    });
    vi.mocked(api.loginCodex).mockResolvedValue({ installed: true, authenticated: true, detail: "Authenticated" });

    render(<Settings onBack={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Sign in with ChatGPT" }));

    expect(api.loginCodex).toHaveBeenCalledOnce();
    expect(await screen.findByText("Sign in again")).toBeInTheDocument();
    expect(screen.getAllByText("Authenticated")).not.toHaveLength(0);
  });

  it("restores and can cancel an in-progress browser sign-in", async () => {
    vi.mocked(api.doctor).mockResolvedValue({
      appDataWritable: true,
      databaseHealthy: true,
      git: { installed: true, detail: "Available" },
      claude: { installed: true, authenticated: true, detail: "Available" },
      codex: { installed: true, authenticated: false, detail: "Login required" },
      os: "macOS",
    });
    vi.mocked(api.codexAuthInProgress).mockResolvedValue(true);
    vi.mocked(api.cancelCodexLogin).mockResolvedValue();

    render(<Settings onBack={vi.fn()} />);
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(api.cancelCodexLogin).toHaveBeenCalledOnce();
    expect(screen.getByRole("status")).toHaveTextContent("Complete sign-in in your browser");
  });

  it("does not turn a sign-in double click into cancellation", async () => {
    vi.mocked(api.doctor).mockResolvedValue({
      appDataWritable: true,
      databaseHealthy: true,
      git: { installed: true, detail: "Available" },
      claude: { installed: true, authenticated: true, detail: "Available" },
      codex: { installed: true, authenticated: false, detail: "Login required" },
      os: "macOS",
    });
    vi.mocked(api.loginCodex).mockReturnValue(new Promise(() => {}));

    render(<Settings onBack={vi.fn()} />);
    const login = await screen.findByRole("button", { name: "Sign in with ChatGPT" });
    fireEvent.click(login);
    fireEvent.click(login);

    expect(api.loginCodex).toHaveBeenCalledOnce();
    expect(api.cancelCodexLogin).not.toHaveBeenCalled();
    expect(login).toBeDisabled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled();
  });
});
