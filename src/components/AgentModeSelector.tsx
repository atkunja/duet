import { Bot, Cloud, Combine, Laptop, Sparkles } from "lucide-react";
import { useId } from "react";

export type AgentMode = "duet" | "codex" | "claude";
export type ExecutionLocation = "local" | "cloud";
export type ReasoningEffort = "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra";

export interface AgentConfiguration {
  model: string;
  reasoning: ReasoningEffort;
}

export interface AgentModeSelection {
  mode: AgentMode;
  location: ExecutionLocation;
  codex: AgentConfiguration;
  claude: AgentConfiguration;
}

export interface ModelOption {
  value: string;
  label: string;
  defaultReasoning?: ReasoningEffort;
  reasoningEfforts?: readonly ReasoningEffort[];
}

export interface AgentModeSelectorProps {
  value: AgentModeSelection;
  onChange: (value: AgentModeSelection) => void;
  cloudSupported?: boolean;
  disabled?: boolean;
  codexModels?: readonly ModelOption[];
  claudeModels?: readonly ModelOption[];
  className?: string;
}

export const defaultAgentModeSelection: AgentModeSelection = {
  mode: "duet",
  location: "local",
  codex: { model: "gpt-5.6-sol", reasoning: "high" },
  claude: { model: "sonnet", reasoning: "high" },
};

const defaultCodexModels: readonly ModelOption[] = [
  { value: "gpt-5.6-sol", label: "GPT-5.6 Sol" },
  { value: "gpt-5.6-terra", label: "GPT-5.6 Terra" },
  { value: "gpt-5.6-luna", label: "GPT-5.6 Luna" },
];

const defaultClaudeModels: readonly ModelOption[] = [
  { value: "opus", label: "Claude Opus" },
  { value: "sonnet", label: "Claude Sonnet" },
  { value: "haiku", label: "Claude Haiku" },
];

const reasoningOptions: readonly { value: ReasoningEffort; label: string }[] = [
  { value: "minimal", label: "Minimal" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "xhigh", label: "Extra high" },
  { value: "max", label: "Maximum" },
  { value: "ultra", label: "Ultra" },
];

const modes: readonly {
  value: AgentMode;
  label: string;
  description: string;
  icon: typeof Combine;
}[] = [
  {
    value: "duet",
    label: "Duet",
    description: "Codex plans and reviews. Claude builds and repairs.",
    icon: Combine,
  },
  {
    value: "codex",
    label: "Codex",
    description: "Codex handles the task from plan through review.",
    icon: Bot,
  },
  {
    value: "claude",
    label: "Claude",
    description: "Claude handles the task from plan through delivery.",
    icon: Sparkles,
  },
];

function AgentControls({
  agent,
  value,
  models,
  disabled,
  idPrefix,
  onChange,
}: {
  agent: "Codex" | "Claude";
  value: AgentConfiguration;
  models: readonly ModelOption[];
  disabled: boolean;
  idPrefix: string;
  onChange: (value: AgentConfiguration) => void;
}) {
  const agentKey = agent.toLowerCase();
  const modelId = `${idPrefix}-${agentKey}-model`;
  const reasoningId = `${idPrefix}-${agentKey}-reasoning`;
  const selectedModel = models.find((model) => model.value === value.model);
  const supportedEfforts = selectedModel?.reasoningEfforts?.length ? selectedModel.reasoningEfforts : reasoningOptions.map(option => option.value);

  return (
    <section className={`agent-mode-selector__agent agent-mode-selector__agent--${agentKey}`} aria-labelledby={`${idPrefix}-${agentKey}-title`}>
      <div className="agent-mode-selector__agent-heading">
        <span className={`agent-mode-selector__agent-mark agent-mode-selector__agent-mark--${agentKey}`} aria-hidden="true">
          {agent === "Codex" ? "CX" : "CL"}
        </span>
        <span id={`${idPrefix}-${agentKey}-title`}>{agent}</span>
      </div>
      <div className="agent-mode-selector__controls">
        <label className="agent-mode-selector__field" htmlFor={modelId}>
          <span>Model</span>
          <select
            id={modelId}
            aria-label={`${agent} model`}
            value={value.model}
            disabled={disabled}
            onChange={(event) => {
              const model = models.find((option) => option.value === event.target.value);
              const supported = model?.reasoningEfforts?.length ? model.reasoningEfforts : reasoningOptions.map(option => option.value);
              const reasoning = supported.includes(value.reasoning) ? value.reasoning : model?.defaultReasoning ?? supported[0] ?? value.reasoning;
              onChange({ model: event.target.value, reasoning });
            }}
          >
            {models.map((model) => <option key={model.value} value={model.value}>{model.label}</option>)}
          </select>
        </label>
        <label className="agent-mode-selector__field" htmlFor={reasoningId}>
          <span>Reasoning</span>
          <select
            id={reasoningId}
            aria-label={`${agent} reasoning`}
            value={value.reasoning}
            disabled={disabled}
            onChange={(event) => onChange({ ...value, reasoning: event.target.value as ReasoningEffort })}
          >
            {reasoningOptions.filter(option => supportedEfforts.includes(option.value)).map((option) => <option key={option.value} value={option.value}>{option.label}</option>)}
          </select>
        </label>
      </div>
    </section>
  );
}

export function AgentModeSelector({
  value,
  onChange,
  cloudSupported = false,
  disabled = false,
  codexModels = defaultCodexModels,
  claudeModels = defaultClaudeModels,
  className = "",
}: AgentModeSelectorProps) {
  const id = useId().replaceAll(":", "");
  const selectedMode = modes.find((mode) => mode.value === value.mode) ?? modes[0];

  return (
    <div className={["agent-mode-selector", className].filter(Boolean).join(" ")} data-mode={value.mode}>
      <fieldset className="agent-mode-selector__section" disabled={disabled}>
        <legend className="agent-mode-selector__legend">Agent mode</legend>
        <div className="agent-mode-selector__modes">
          {modes.map((mode) => {
            const Icon = mode.icon;
            const descriptionId = `${id}-${mode.value}-description`;
            return (
              <label className={`agent-mode-selector__mode${value.mode === mode.value ? " agent-mode-selector__mode--selected" : ""}`} key={mode.value}>
                <input
                  className="agent-mode-selector__radio"
                  type="radio"
                  name={`${id}-mode`}
                  value={mode.value}
                  checked={value.mode === mode.value}
                  aria-label={mode.label}
                  aria-describedby={descriptionId}
                  onChange={() => onChange({ ...value, mode: mode.value })}
                />
                <Icon className="agent-mode-selector__mode-icon" size={16} aria-hidden="true" />
                <span className="agent-mode-selector__mode-copy">
                  <strong>{mode.label}</strong>
                  <small id={descriptionId}>{mode.description}</small>
                </span>
                <span className="agent-mode-selector__check" aria-hidden="true" />
              </label>
            );
          })}
        </div>
      </fieldset>

      <div className="agent-mode-selector__details" aria-live="polite">
        <p className="agent-mode-selector__summary">
          <span>{selectedMode.label}</span>
          <small>{selectedMode.description}</small>
        </p>
        <div className={`agent-mode-selector__agents agent-mode-selector__agents--${value.mode}`}>
          {value.mode !== "claude" ? (
            <AgentControls
              agent="Codex"
              value={value.codex}
              models={codexModels}
              disabled={disabled}
              idPrefix={id}
              onChange={(codex) => onChange({ ...value, codex })}
            />
          ) : null}
          {value.mode !== "codex" ? (
            <AgentControls
              agent="Claude"
              value={value.claude}
              models={claudeModels}
              disabled={disabled}
              idPrefix={id}
              onChange={(claude) => onChange({ ...value, claude })}
            />
          ) : null}
        </div>
      </div>

      <fieldset className="agent-mode-selector__section agent-mode-selector__location" disabled={disabled}>
        <legend className="agent-mode-selector__legend">Execution location</legend>
        <div className="agent-mode-selector__location-options">
          <label className={`agent-mode-selector__location-option${value.location === "local" ? " agent-mode-selector__location-option--selected" : ""}`}>
            <input
              className="agent-mode-selector__radio"
              type="radio"
              name={`${id}-location`}
              value="local"
              checked={value.location === "local"}
              onChange={() => onChange({ ...value, location: "local" })}
            />
            <Laptop size={14} aria-hidden="true" />
            <span>Local</span>
          </label>
          <label className={`agent-mode-selector__location-option${value.location === "cloud" ? " agent-mode-selector__location-option--selected" : ""}${cloudSupported ? "" : " agent-mode-selector__location-option--unavailable"}`}>
            <input
              className="agent-mode-selector__radio"
              type="radio"
              name={`${id}-location`}
              value="cloud"
              checked={value.location === "cloud"}
              disabled={!cloudSupported}
              aria-label="Cloud"
              aria-describedby={cloudSupported ? undefined : `${id}-cloud-status`}
              onChange={() => {
                if (cloudSupported) onChange({ ...value, location: "cloud" });
              }}
            />
            <Cloud size={14} aria-hidden="true" />
            <span>Cloud</span>
            {!cloudSupported ? <small id={`${id}-cloud-status`} className="agent-mode-selector__coming-soon">Coming soon</small> : null}
          </label>
        </div>
      </fieldset>
    </div>
  );
}
