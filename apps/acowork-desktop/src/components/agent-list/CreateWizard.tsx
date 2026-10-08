import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cn } from "../../lib/utils";
import {
  Sparkles,
  Bot,
  Wrench,
  PackagePlus,
  Check,
  Loader2,
  X as XIcon,
  Trash2,
} from "lucide-react";
import { StyledInput, StyledTextarea } from "../common/StyledInput";
import { useEscapeClose } from "../../hooks/useEscapeClose";
import { Switch } from "../common/Switch";
import { ListBox, ListRow } from "../common/list";
import { useTranslation } from "../../i18n/useTranslation";
import { ErrorBox } from "../common/ErrorBox";

interface CreateWizardProps {
  open: boolean;
  onCreated: (agentId: string) => void;
  onClose: () => void;
}

type WizardStep = "basic" | "tools" | "skills" | "preview";

type StepIcon = { key: WizardStep; icon: React.ElementType; i18nKey: string };
const STEP_ICONS: StepIcon[] = [
  { key: "basic", icon: Bot, i18nKey: "Basic" },
  { key: "tools", icon: Wrench, i18nKey: "Tools" },
  { key: "skills", icon: PackagePlus, i18nKey: "Skills" },
  { key: "preview", icon: Check, i18nKey: "Preview" },
];

interface BuiltinToolInfo {
  name: string;
  description: string;
  group?: string;
}

interface SkillDraft {
  /** Caller-supplied id so React lists stay stable across re-parses. */
  uid: string;
  /** Original filename for display. */
  fileName: string;
  /** Raw zip bytes — passed to `create_agent` as `skillFiles`. */
  bytes: Uint8Array;
  /** `null` while parsing or after a parse error. */
  parsed: { name: string; description: string } | null;
  /** Populated when parsing failed; user sees it in the row. */
  error: string | null;
}

interface AgentFormData {
  agent_id: string;
  name: string;
  version: string;
  description: string;
  author: string;
}

const DEFAULT_FORM: AgentFormData = {
  agent_id: "",
  name: "",
  version: "0.1.0",
  description: "",
  author: "",
};

export function CreateWizard({ open, onCreated, onClose }: CreateWizardProps) {
  const { t } = useTranslation();
  const STEPS = STEP_ICONS.map((s) => ({ ...s, label: t(`createWizard.step${s.i18nKey}`) }));
  const [step, setStep] = useState<WizardStep>("basic");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [form, setForm] = useState<AgentFormData>({ ...DEFAULT_FORM });
  const [builtinTools, setBuiltinTools] = useState<BuiltinToolInfo[]>([]);
  const [selectedTools, setSelectedTools] = useState<Set<string>>(new Set());
  const [skills, setSkills] = useState<SkillDraft[]>([]);

  // Reset on open
  useEffect(() => {
    if (open) {
      setStep("basic");
      setError(null);
      setForm({ ...DEFAULT_FORM });
      setSelectedTools(new Set());
      setSkills([]);
    }
  }, [open]);

  // Load builtin tool catalog once per wizard open. The list is static
  // and shared with the Runtime (BUILTIN_TOOLS const in acowork-core),
  // so caching it in component state is fine.
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    invoke<BuiltinToolInfo[]>("list_builtin_tools")
      .then((list) => {
        if (!cancelled) setBuiltinTools(list);
      })
      .catch((e) => {
        if (!cancelled) setError(`Failed to load builtin tool catalog: ${e}`);
      });
    return () => {
      cancelled = true;
    };
  }, [open]);

  // Close on Escape — `!busy` is the gate so a wizard mid-submit cannot be
  // dismissed out from under itself. The hook reads it at keydown time,
  // so a spinner flip no longer re-subscribes the listener.
  useEscapeClose(open, onClose, !busy);

  const update = (patch: Partial<AgentFormData>) =>
    setForm((prev) => ({ ...prev, ...patch }));

  const stepIndex = STEPS.findIndex((s) => s.key === step);
  const canNext = () => {
    switch (step) {
      case "basic":
        return form.agent_id.trim() !== "" && form.name.trim() !== "";
      case "tools":
        // Tools is optional (empty selection is valid) but we still
        // need the catalog to be loaded so the user can see the list.
        return builtinTools.length > 0;
      case "skills":
        // Skills is optional AND every uploaded zip must have parsed
        // successfully — a half-parsed draft blocks progress so the
        // user notices before the final create.
        return skills.every((s) => s.parsed !== null && s.error === null);
      default:
        return true;
    }
  };

  const handleNext = () => {
    if (step === "preview") {
      handleCreate();
      return;
    }
    const nextIdx = stepIndex + 1;
    if (nextIdx < STEPS.length) setStep(STEPS[nextIdx].key);
  };

  const handleBack = () => {
    const prevIdx = stepIndex - 1;
    if (prevIdx >= 0) setStep(STEPS[prevIdx].key);
  };

  const toggleTool = (name: string) => {
    setSelectedTools((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  };

  /**
   * Add one or more files dropped/selected from the Skills step drop
   * zone. We send each ZIP to the backend in parallel — the backend
   * reuses `parse_skill_zip` to extract `name` + `description`, so
   * the wizard never needs a JS zip library.
   */
  const addSkillFiles = async (files: File[]) => {
    const drafts: SkillDraft[] = files.map((f) => ({
      uid: `${f.name}-${f.size}-${f.lastModified}-${Math.random().toString(36).slice(2, 8)}`,
      fileName: f.name,
      bytes: new Uint8Array(0),
      parsed: null,
      error: null,
    }));
    setSkills((prev) => [...prev, ...drafts]);

    await Promise.all(
      files.map(async (file, i) => {
        const draft = drafts[i];
        try {
          const buf = new Uint8Array(await file.arrayBuffer());
          const preview = await invoke<{ name: string; description: string }>(
            "parse_skill_zip_preview",
            { zipBytes: Array.from(buf) },
          );
          setSkills((prev) =>
            prev.map((s) =>
              s.uid === draft.uid
                ? { ...s, bytes: buf, parsed: preview, error: null }
                : s,
            ),
          );
        } catch (e) {
          setSkills((prev) =>
            prev.map((s) =>
              s.uid === draft.uid
                ? { ...s, error: String(e) }
                : s,
            ),
          );
        }
      }),
    );
  };

  const removeSkill = (uid: string) => {
    setSkills((prev) => prev.filter((s) => s.uid !== uid));
  };

  const handleCreate = async () => {
    setBusy(true);
    setError(null);
    try {
      const agentId = await invoke<string>("create_agent", {
        agentId: form.agent_id.trim(),
        name: form.name.trim(),
        version: form.version || null,
        description: form.description || null,
        author: form.author || null,
        tools: Array.from(selectedTools),
        skillFiles: skills.map((s) => Array.from(s.bytes)),
      });
      onCreated(agentId);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!open) return null;

  const nextLabel = step === "preview" ? t("createWizard.buttonCreateAgent") : t("createWizard.buttonNext");
  const canProceed = canNext() && !busy;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-modal-overlay"
        onClick={busy ? undefined : onClose}
      />

      {/* Dialog */}
      <div className="relative z-10 flex max-h-[90vh] w-full max-w-2xl flex-col rounded-md border border-border-outer bg-modal-surface shadow-xl">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border-divider px-5 py-3 min-h-[var(--ui-dialog-zone-h)]">
          <div className="flex items-center gap-2">
            <Sparkles className="h-5 w-5 text-text-tertiary " />
            <h2 className="text-sm font-semibold text-text ">
              Create New Agent
            </h2>
          </div>
          {/* X closes from any step (not just step 1) — the footer
              "Back" button only navigates between steps, not
              dismisses. */}
          <button
            onClick={busy ? undefined : onClose}
            disabled={busy}
            aria-label={t("agentDetailDialog.ariaLabelClose")}
            className="text-text-tertiary hover:text-zinc-600 disabled:opacity-50 dark:hover:text-zinc-300"
          >
            <XIcon className="h-4 w-4" />
          </button>
        </div>

        {/* Step indicators */}
        <div className="flex items-center gap-0 border-b border-border-divider px-5 min-h-[var(--ui-dialog-zone-h)]">
          {STEPS.map((s, i) => {
            const Icon = s.icon;
            const active = s.key === step;
            const passed = i < stepIndex;
            return (
              <div key={s.key} className="flex items-center">
                <div
                  className={cn(
                    "flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition-colors",
                    active &&
                      "bg-zinc-200 text-text dark:bg-zinc-700 ",
                    passed &&
                      "bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400",
                    !active && !passed && "text-text-tertiary ",
                  )}
                >
                  {passed ? (
                    <Check className="h-3 w-3" />
                  ) : (
                    <Icon className="h-3 w-3" />
                  )}
                  {s.label}
                </div>
                {i < STEPS.length - 1 && (
                  <div
                    className={cn(
                      "mx-1 h-px w-4",
                      i < stepIndex
                        ? "bg-green-300 dark:bg-green-600"
                        : "bg-zinc-200 dark:bg-zinc-600",
                    )}
                  />
                )}
              </div>
            );
          })}
        </div>

        {/* Step content — `min-h-0` is what lets the flex child shrink
             below its content size, so `overflow-y-auto` actually
             triggers instead of pushing the dialog off-screen
             (well-known flexbox quirk). */}
        <div className="flex-1 min-h-0 space-y-4 overflow-y-auto px-5 py-4">
          {/* Step 1: Basic info */}
          {step === "basic" && (
            <div className="space-y-3">
              <div>
                <label className="mb-1 block text-xs font-medium text-text-tertiary ">
                  Agent ID * <span className="font-normal text-text-tertiary">(e.g. com.example.myagent)</span>
                </label>
                <StyledInput
                  type="text"
                  value={form.agent_id}
                  onChange={(e) => update({ agent_id: e.target.value })}
                  placeholder="com.example.myagent"
                  className=""
                />
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-tertiary ">
                  Display Name *
                </label>
                <StyledInput
                  type="text"
                  value={form.name}
                  onChange={(e) => update({ name: e.target.value })}
                  placeholder={t("createWizard.placeholderAgentName")}
                  className=""
                />
              </div>
              <div className="grid grid-cols-2 gap-3">
                <div>
                  <label className="mb-1 block text-xs font-medium text-text-tertiary ">
                    Version
                  </label>
                  <StyledInput
                    type="text"
                    value={form.version}
                    onChange={(e) => update({ version: e.target.value })}
                    placeholder="0.1.0"
                    className=""
                  />
                </div>
                <div>
                  <label className="mb-1 block text-xs font-medium text-text-tertiary ">
                    Author
                  </label>
                  <StyledInput
                    type="text"
                    value={form.author}
                    onChange={(e) => update({ author: e.target.value })}
                    placeholder={t("createWizard.placeholderYourName")}
                    className=""
                  />
                </div>
              </div>
              <div>
                <label className="mb-1 block text-xs font-medium text-text-tertiary ">
                  Description
                </label>
                <StyledTextarea
                  value={form.description}
                  onChange={(e) => update({ description: e.target.value })}
                  placeholder={t("createWizard.placeholderDescribe")}
                  rows={3}
                  className="resize-none"
                />
              </div>
            </div>
          )}

          {/* Step 2: Builtin tools (multi-select).
              Row contract mirrors the right-panel "Builtin Tools"
              list (ToolsTab.tsx): one ListRow per tool, name on the
              left, unified <Switch> on the right. `selected` +
              Switch `checked` carry the same meaning so the visual
              treatment stays consistent across the app. The ListBox
              is `plain` (sits on the dialog surface) and `maxHeight`
              so 20 entries scroll inside the step instead of
              pushing the dialog off-screen. */}
          {step === "tools" && (
            <div className="space-y-3">
              <p className="text-xs text-text-tertiary ">
                {t("createWizard.toolsHint")}
              </p>
              {builtinTools.length === 0 ? (
                <div className="text-xs text-text-tertiary">Loading…</div>
              ) : (
                <ListBox variant="plain" maxHeight={280}>
                  {builtinTools.map((tool) => {
                    const checked = selectedTools.has(tool.name);
                    return (
                      <ListRow
                        key={tool.name}
                        selected={checked}
                        padding="default"
                        trailing={
                          <Switch
                            checked={checked}
                            onChange={() => toggleTool(tool.name)}
                            disabled={busy}
                            size="sm"
                            aria-label={tool.name}
                          />
                        }
                        onClick={() => toggleTool(tool.name)}
                      >
                        <div className="min-w-0 flex-1">
                          <p className="truncate font-mono text-11 font-medium text-text-secondary ">
                            {tool.name}
                          </p>
                          <p className="truncate text-10 text-text-tertiary ">
                            {tool.description}
                          </p>
                        </div>
                      </ListRow>
                    );
                  })}
                </ListBox>
              )}
            </div>
          )}

          {/* Step 3: Skills (drop-zone import) */}
          {step === "skills" && (
            <SkillsStep
              skills={skills}
              onAdd={addSkillFiles}
              onRemove={removeSkill}
            />
          )}

          {/* Step 4: Preview.
              The rendered TOML scales with the number of tools +
              capabilities, so we cap the <pre> at ~360px and scroll
              it internally. Without this, a 20-tool agent's preview
              would push the dialog's outer step-content scroll past
              the footer. `ponytail:` capping the inner body is
              strictly cheaper than asking the user to scroll the
              whole dialog; the step content's own overflow-y-auto
              stays as a safety net for tall single-step bodies. */}
          {step === "preview" && (
            <div className="space-y-3">
              <h3 className="text-xs font-medium text-text-secondary ">
                {t("createWizard.previewTitle")}
              </h3>
              <div className="max-h-[360px] overflow-y-auto rounded-md bg-zinc-50 px-4 py-3 dark:bg-zinc-700/50">
                <pre className="whitespace-pre-wrap text-xs text-text-secondary ">
                  {buildPreviewText(form, selectedTools, skills)}
                </pre>
              </div>
            </div>
          )}

          {/* Error */}
          {error && (
            <ErrorBox message={error} onClose={() => setError(null)} />
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between border-t border-border-divider px-5 min-h-[var(--ui-dialog-zone-h)]">
          <button
            onClick={stepIndex === 0 ? onClose : handleBack}
            disabled={busy}
            className="rounded-md px-3 py-1.5 text-xs font-medium text-text-secondary hover:bg-zinc-100 disabled:opacity-50  dark:hover:bg-zinc-700"
          >
            {stepIndex === 0 ? t("common.cancel") : t("common.back")}
          </button>

          <button
            onClick={handleNext}
            disabled={!canProceed}
            className="flex items-center gap-2 rounded btn-accent px-3 py-1.5 text-xs font-medium disabled:cursor-not-allowed disabled:opacity-50"
          >
            {busy ? (
              <>
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
                Creating...
              </>
            ) : (
              nextLabel
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* SkillsStep sub-component                                                   */
/* -------------------------------------------------------------------------- */

function SkillsStep({
  skills,
  onAdd,
  onRemove,
}: {
  skills: SkillDraft[];
  onAdd: (files: File[]) => Promise<void>;
  onRemove: (uid: string) => void;
}) {
  const { t } = useTranslation();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [dragging, setDragging] = useState(false);

  const handleFiles = (fileList: FileList | null) => {
    if (!fileList || fileList.length === 0) return;
    void onAdd(Array.from(fileList));
  };

  return (
    <div className="space-y-3">
      <p className="text-xs text-text-tertiary ">
        {t("createWizard.skillsHint")}
      </p>
      <div
        onDrop={(e) => {
          e.preventDefault();
          setDragging(false);
          handleFiles(e.dataTransfer.files);
        }}
        onDragOver={(e) => {
          e.preventDefault();
          setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onClick={() => fileInputRef.current?.click()}
        className={cn(
          "cursor-pointer rounded-md border-2 border-dashed p-5 text-center transition-colors",
          dragging
            ? "border-zinc-400 bg-zinc-50 dark:border-zinc-500 dark:bg-zinc-700/40"
            : "border-zinc-300 hover:border-zinc-400 dark:border-zinc-600 dark:hover:border-zinc-500",
        )}
      >
        <input
          ref={fileInputRef}
          type="file"
          accept=".zip"
          multiple
          onChange={(e) => handleFiles(e.target.files)}
          className="hidden"
        />
        <PackagePlus className="mx-auto mb-2 h-6 w-6 text-text-tertiary" />
        <p className="text-xs text-text-tertiary">
          {t("createWizard.skillsDropzonePrompt")}
        </p>
      </div>

      {skills.length > 0 && (
        <ListBox variant="plain" maxHeight={200}>
          {skills.map((s) => {
            const ok = s.parsed !== null && s.error === null;
            return (
              <ListRow
                key={s.uid}
                padding="default"
                className={cn(
                  !ok && "bg-red-50 dark:bg-red-900/20",
                )}
                trailing={
                  <button
                    onClick={() => onRemove(s.uid)}
                    aria-label={t("createWizard.removeSkillAriaLabel")}
                    className="shrink-0 rounded p-1 text-text-tertiary hover:bg-zinc-100 hover:text-text dark:hover:bg-zinc-700"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                  </button>
                }
              >
                <div className="min-w-0 flex-1">
                  <p className="truncate font-mono text-11 font-medium text-text-secondary ">
                    {ok ? s.parsed!.name : s.fileName}
                  </p>
                  <p className="truncate text-10 text-text-tertiary ">
                    {ok ? s.parsed!.description : s.error ?? "Parsing…"}
                  </p>
                </div>
              </ListRow>
            );
          })}
        </ListBox>
      )}
    </div>
  );
}

/* -------------------------------------------------------------------------- */
/* Preview text builder                                                        */
/* -------------------------------------------------------------------------- */

function buildPreviewText(
  form: AgentFormData,
  selectedTools: Set<string>,
  skills: SkillDraft[],
): string {
  const lines: string[] = [];
  lines.push("[package]");
  lines.push(`agent_id = "${form.agent_id}"`);
  lines.push(`name = "${form.name}"`);
  lines.push(`version = "${form.version}"`);
  lines.push(`description = "${form.description || "(none)"}"`);
  lines.push(`author = "${form.author || "(none)"}"`);
  lines.push(`runtime_version = "0.1.0"`);
  lines.push(`dev = true`);
  if (selectedTools.size > 0) {
    lines.push("");
    for (const t of selectedTools) {
      lines.push("[[tools]]");
      lines.push(`name = "${t}"`);
    }
  }
  const validSkills = skills.filter((s) => s.parsed);
  if (validSkills.length > 0) {
    lines.push("");
    for (const s of validSkills) {
      lines.push(`[capabilities.${s.parsed!.name}]`);
      lines.push(`description = ${JSON.stringify(s.parsed!.description)}`);
    }
  }
  return lines.join("\n");
}
