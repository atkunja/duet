import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { AgentModeSelector, defaultAgentModeSelection } from "./AgentModeSelector";

describe("AgentModeSelector", () => {
  it("exposes all modes and emits a complete controlled selection", () => {
    const onChange = vi.fn();
    render(<AgentModeSelector value={defaultAgentModeSelection} onChange={onChange} />);

    expect(screen.getByRole("radio", { name: "Duet" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "Codex" })).not.toBeChecked();
    expect(screen.getByRole("radio", { name: "Claude" })).not.toBeChecked();

    fireEvent.click(screen.getByRole("radio", { name: "Codex" }));
    expect(onChange).toHaveBeenCalledWith({ ...defaultAgentModeSelection, mode: "codex" });
  });

  it("shows only relevant agent controls and keeps their labels accessible", () => {
    const onChange = vi.fn();
    const { rerender } = render(
      <AgentModeSelector value={{ ...defaultAgentModeSelection, mode: "codex" }} onChange={onChange} />,
    );

    expect(screen.getByRole("combobox", { name: "Codex model" })).toHaveValue("gpt-5.6-sol");
    expect(screen.getByRole("combobox", { name: "Codex reasoning" })).toHaveValue("high");
    expect(screen.queryByRole("region", { name: "Claude" })).not.toBeInTheDocument();

    rerender(<AgentModeSelector value={{ ...defaultAgentModeSelection, mode: "duet" }} onChange={onChange} />);
    expect(screen.getByRole("region", { name: "Codex" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Claude" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Codex model" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Codex reasoning" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Claude model" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Claude reasoning" })).toBeInTheDocument();
  });

  it("updates model and reasoning settings without dropping the rest of the selection", () => {
    const onChange = vi.fn();
    render(
      <AgentModeSelector
        value={{ ...defaultAgentModeSelection, mode: "codex" }}
        onChange={onChange}
        codexModels={[{ value: "custom-codex", label: "Custom Codex" }]}
      />,
    );

    fireEvent.change(screen.getByRole("combobox", { name: "Codex model" }), { target: { value: "custom-codex" } });
    expect(onChange).toHaveBeenLastCalledWith({
      ...defaultAgentModeSelection,
      mode: "codex",
      codex: { ...defaultAgentModeSelection.codex, model: "custom-codex" },
    });

    fireEvent.change(screen.getByRole("combobox", { name: "Codex reasoning" }), { target: { value: "xhigh" } });
    expect(onChange).toHaveBeenLastCalledWith({
      ...defaultAgentModeSelection,
      mode: "codex",
      codex: { ...defaultAgentModeSelection.codex, reasoning: "xhigh" },
    });
  });

  it("marks cloud as coming soon and prevents selection until supported", () => {
    const onChange = vi.fn();
    const { rerender } = render(<AgentModeSelector value={defaultAgentModeSelection} onChange={onChange} />);
    const unavailableCloud = screen.getByRole("radio", { name: /cloud/i });

    expect(unavailableCloud).toBeDisabled();
    expect(screen.getByText("Coming soon")).toBeVisible();
    fireEvent.click(unavailableCloud);
    expect(onChange).not.toHaveBeenCalled();

    rerender(<AgentModeSelector value={defaultAgentModeSelection} onChange={onChange} cloudSupported />);
    const availableCloud = screen.getByRole("radio", { name: "Cloud" });
    expect(availableCloud).toBeEnabled();
    expect(screen.queryByText("Coming soon")).not.toBeInTheDocument();
    fireEvent.click(availableCloud);
    expect(onChange).toHaveBeenCalledWith({ ...defaultAgentModeSelection, location: "cloud" });
  });

  it("disables every interactive control when the selector is disabled", () => {
    render(<AgentModeSelector value={defaultAgentModeSelection} onChange={vi.fn()} disabled cloudSupported />);
    for (const control of [...screen.getAllByRole("radio"), ...screen.getAllByRole("combobox")]) {
      expect(control).toBeDisabled();
    }
  });
});
