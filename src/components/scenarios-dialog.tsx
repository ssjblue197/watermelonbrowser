"use client";

import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  LuBan,
  LuChevronDown,
  LuChevronRight,
  LuCode,
  LuPlay,
  LuPlus,
  LuRefreshCw,
  LuSave,
  LuTrash2,
} from "react-icons/lu";
import { BlockEditor } from "@/components/block-editor";
import { DeleteConfirmationDialog } from "@/components/delete-confirmation-dialog";
import MultipleSelector, { type Option } from "@/components/multiple-selector";
import { AnimatedSwitch } from "@/components/ui/animated-switch";
import {
  AnimatedTabs,
  AnimatedTabsContent,
  AnimatedTabsList,
  AnimatedTabsTrigger,
} from "@/components/ui/animated-tabs";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import { showErrorToast, showSuccessToast } from "@/lib/toast-utils";
import type {
  BrowserProfile,
  Scenario,
  ScenarioAiConfigView,
  ScenarioAiProvider,
  ScenarioProfileAssignment,
  ScenarioRotationMode,
  ScenarioRunDetail,
  ScenarioRunInfo,
  ScenarioRunSummary,
  ScenarioSchedule,
  ScenarioTriggerType,
} from "@/types";

interface ScenariosDialogProps {
  isOpen: boolean;
  onClose: () => void;
  subPage?: boolean;
  profiles: BrowserProfile[];
  runningProfiles: Set<string>;
}

function uuid(): string {
  try {
    return crypto.randomUUID();
  } catch {
    return `scn-${Date.now()}-${Math.floor(Math.random() * 1e6)}`;
  }
}

function newScenario(): Scenario {
  return {
    id: uuid(),
    name: "New scenario",
    description: "",
    blocks: [
      { type: "open_url", params: { url: "https://example.com" } },
      { type: "get_page_text", params: { output_variable: "page" } },
    ],
  };
}

function newScenarioJson(): string {
  return JSON.stringify(newScenario(), null, 2);
}

function newScheduleJson(): string {
  return JSON.stringify(
    {
      id: uuid(),
      scenario_id: "",
      name: "New schedule",
      enabled: false,
      trigger_type: "interval",
      interval_minutes: 60,
      time_window_start: null,
      time_window_end: null,
      max_runs_per_day: null,
    },
    null,
    2,
  );
}

function assignmentJsonFor(scheduleId: string): string {
  return JSON.stringify(
    {
      schedule_id: scheduleId,
      profile_ids: [],
      group_ids: [],
      rotation_mode: "round_robin",
      max_parallel: 1,
      cooldown_minutes: 0,
      run_headless: false,
    },
    null,
    2,
  );
}

const PROVIDERS: ScenarioAiProvider[] = [
  "anthropic",
  "openai",
  "gemini",
  "ollama",
];

const TRIGGER_TYPES: ScenarioTriggerType[] = [
  "interval",
  "cron",
  "manual",
  "on_event",
];

const ROTATION_MODES: ScenarioRotationMode[] = [
  "round_robin",
  "random",
  "least_used",
  "all_parallel",
];

/** Small section heading used inside the form panels. */
function FieldLabel({ children }: { children: React.ReactNode }) {
  return (
    <span className="text-[11px] font-medium text-muted-foreground">
      {children}
    </span>
  );
}

function statusTone(status: string): string {
  if (status === "success") return "bg-success/15 text-success";
  if (status === "failed") return "bg-destructive/15 text-destructive";
  return "bg-warning/15 text-warning";
}

export function ScenariosDialog({
  isOpen,
  onClose,
  subPage,
  profiles,
  runningProfiles,
}: ScenariosDialogProps) {
  const { t } = useTranslation();

  // ----- Editor tab -----
  const [scenarios, setScenarios] = useState<Scenario[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  // editorScenario is the source of truth for Visual mode; editorJson for JSON
  // mode. They are synced when switching modes / selecting / creating.
  const [editorScenario, setEditorScenario] = useState<Scenario>(newScenario);
  const [editorJson, setEditorJson] = useState<string>(newScenarioJson());
  const [editorMode, setEditorMode] = useState<"visual" | "json">("visual");
  const [runProfileId, setRunProfileId] = useState<string>("");
  const [isRunning, setIsRunning] = useState(false);

  // Confirm-before-delete for scenarios and schedules.
  const [pendingDelete, setPendingDelete] = useState<{
    kind: "scenario" | "schedule";
    id: string;
    name: string;
  } | null>(null);
  const [isDeleting, setIsDeleting] = useState(false);

  // ----- Runs tab -----
  const [runs, setRuns] = useState<ScenarioRunSummary[]>([]);
  const [activeRuns, setActiveRuns] = useState<ScenarioRunInfo[]>([]);
  const [runDetail, setRunDetail] = useState<ScenarioRunDetail | null>(null);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);

  // ----- Schedules tab -----
  const [schedules, setSchedules] = useState<ScenarioSchedule[]>([]);
  const [selectedScheduleId, setSelectedScheduleId] = useState<string | null>(
    null,
  );
  const [scheduleJson, setScheduleJson] = useState<string>(newScheduleJson());
  const [assignmentJson, setAssignmentJson] = useState<string>(
    assignmentJsonFor(""),
  );
  const [showScheduleJson, setShowScheduleJson] = useState(false);

  // ----- AI tab -----
  const [aiProvider, setAiProvider] = useState<ScenarioAiProvider>("anthropic");
  const [aiModel, setAiModel] = useState<string>("claude-haiku-4-5");
  const [aiApiKey, setAiApiKey] = useState<string>("");
  const [aiBaseUrl, setAiBaseUrl] = useState<string>("");
  const [aiMaxTokens, setAiMaxTokens] = useState<string>("1024");
  const [aiTemperature, setAiTemperature] = useState<string>("0.3");
  const [aiHasKey, setAiHasKey] = useState(false);

  const runnableProfiles = useMemo(
    () =>
      profiles.filter(
        (p) =>
          (p.browser === "wayfern" || p.browser === "camoufox") &&
          runningProfiles.has(p.id),
      ),
    [profiles, runningProfiles],
  );

  const profileName = useCallback(
    (id: string) => profiles.find((p) => p.id === id)?.name ?? id,
    [profiles],
  );

  const scenarioName = useCallback(
    (id: string) => scenarios.find((s) => s.id === id)?.name ?? id,
    [scenarios],
  );

  const loadScenarios = useCallback(async () => {
    try {
      setScenarios(await invoke<Scenario[]>("scenario_list"));
    } catch (err) {
      showErrorToast(t("scenarios.errors.load", { error: String(err) }));
    }
  }, [t]);

  const loadRuns = useCallback(async () => {
    try {
      const [list, active] = await Promise.all([
        invoke<ScenarioRunSummary[]>("scenario_list_runs", { limit: 100 }),
        invoke<ScenarioRunInfo[]>("scenario_active_runs"),
      ]);
      setRuns(list);
      setActiveRuns(active);
    } catch (err) {
      showErrorToast(t("scenarios.errors.load", { error: String(err) }));
    }
  }, [t]);

  const loadSchedules = useCallback(async () => {
    try {
      setSchedules(await invoke<ScenarioSchedule[]>("scenario_list_schedules"));
    } catch (err) {
      showErrorToast(t("scenarios.errors.load", { error: String(err) }));
    }
  }, [t]);

  const loadAiConfig = useCallback(async () => {
    try {
      const cfg = await invoke<ScenarioAiConfigView | null>(
        "scenario_get_ai_config",
      );
      if (cfg) {
        setAiProvider(cfg.provider);
        setAiModel(cfg.model);
        setAiBaseUrl(cfg.base_url ?? "");
        setAiMaxTokens(String(cfg.max_tokens));
        setAiTemperature(String(cfg.temperature));
        setAiHasKey(cfg.has_api_key);
      }
    } catch (err) {
      showErrorToast(t("scenarios.errors.load", { error: String(err) }));
    }
  }, [t]);

  // Initial load when opened.
  useEffect(() => {
    if (!isOpen) return;
    void loadScenarios();
    void loadRuns();
    void loadSchedules();
    void loadAiConfig();
  }, [isOpen, loadScenarios, loadRuns, loadSchedules, loadAiConfig]);

  // Poll active runs every 3s while open so a running scenario is visible.
  // When the active count drops, a run just finished → refresh history too.
  const prevActiveCount = useRef(0);
  useEffect(() => {
    if (!isOpen) return;
    const id = setInterval(() => {
      void invoke<ScenarioRunInfo[]>("scenario_active_runs")
        .then((active) => {
          setActiveRuns(active);
          if (active.length < prevActiveCount.current) void loadRuns();
          prevActiveCount.current = active.length;
        })
        .catch(() => {});
    }, 3000);
    return () => clearInterval(id);
  }, [isOpen, loadRuns]);

  const selectScenario = useCallback(
    async (id: string) => {
      try {
        const s = await invoke<Scenario | null>("scenario_get", {
          scenarioId: id,
        });
        if (s) {
          setSelectedId(id);
          setEditorScenario(s);
          setEditorJson(JSON.stringify(s, null, 2));
        }
      } catch (err) {
        showErrorToast(t("scenarios.errors.load", { error: String(err) }));
      }
    },
    [t],
  );

  const handleNew = useCallback(() => {
    const s = newScenario();
    setSelectedId(null);
    setEditorScenario(s);
    setEditorJson(JSON.stringify(s, null, 2));
  }, []);

  // Switch Visual ↔ JSON, syncing the two representations.
  const switchMode = useCallback(
    (mode: "visual" | "json") => {
      if (mode === editorMode) return;
      if (mode === "json") {
        setEditorJson(JSON.stringify(editorScenario, null, 2));
      } else {
        try {
          setEditorScenario(JSON.parse(editorJson) as Scenario);
        } catch (err) {
          showErrorToast(t("scenarios.errors.json", { error: String(err) }));
          return;
        }
      }
      setEditorMode(mode);
    },
    [editorMode, editorScenario, editorJson, t],
  );

  // Current scenario for save/run/delete, from whichever editor is active.
  const getScenario = useCallback((): Scenario | null => {
    let obj: Scenario;
    if (editorMode === "json") {
      try {
        obj = JSON.parse(editorJson) as Scenario;
      } catch (err) {
        showErrorToast(t("scenarios.errors.json", { error: String(err) }));
        return null;
      }
    } else {
      obj = editorScenario;
    }
    if (!obj.id || !obj.name) {
      showErrorToast(t("scenarios.errors.idName"));
      return null;
    }
    return obj;
  }, [editorMode, editorJson, editorScenario, t]);

  const handleSaveScenario = useCallback(async () => {
    const scenario = getScenario();
    if (!scenario) return;
    try {
      await invoke("scenario_save", { scenario });
      setSelectedId(scenario.id);
      showSuccessToast(t("scenarios.saved"));
      await loadScenarios();
    } catch (err) {
      showErrorToast(t("scenarios.errors.save", { error: String(err) }));
    }
  }, [getScenario, loadScenarios, t]);

  // Delete operates on the saved selection (selectedId), not the editor buffer,
  // so editing the id field or holding invalid JSON can't retarget the delete.
  const requestDeleteScenario = useCallback(() => {
    if (!selectedId) return;
    setPendingDelete({
      kind: "scenario",
      id: selectedId,
      name: scenarioName(selectedId),
    });
  }, [selectedId, scenarioName]);

  const handleRun = useCallback(async () => {
    const scenario = getScenario();
    if (!scenario) return;
    if (!runProfileId) {
      showErrorToast(t("scenarios.errors.noProfile"));
      return;
    }
    setIsRunning(true);
    try {
      const summary = await invoke<{ status: string; run_id: string }>(
        "scenario_run",
        { profileId: runProfileId, scenario },
      );
      showSuccessToast(t("scenarios.runFinished", { status: summary.status }), {
        description: summary.run_id,
      });
      await loadRuns();
    } catch (err) {
      showErrorToast(t("scenarios.errors.run", { error: String(err) }));
    } finally {
      setIsRunning(false);
    }
  }, [getScenario, runProfileId, loadRuns, t]);

  const openRunDetail = useCallback(
    async (id: string) => {
      setSelectedRunId(id);
      try {
        setRunDetail(
          await invoke<ScenarioRunDetail | null>("scenario_get_run", {
            runId: id,
          }),
        );
      } catch (err) {
        showErrorToast(t("scenarios.errors.load", { error: String(err) }));
      }
    },
    [t],
  );

  const handleCancelRun = useCallback(
    async (id: string) => {
      try {
        await invoke<boolean>("scenario_cancel_run", { runId: id });
        await loadRuns();
      } catch (err) {
        showErrorToast(t("scenarios.errors.cancel", { error: String(err) }));
      }
    },
    [loadRuns, t],
  );

  // ----- Schedules handlers -----
  // The two JSON strings stay the canonical source of truth (save handlers read
  // them verbatim). The form below parses them for editing and re-serialises on
  // every change, so the backend contract is byte-identical to the raw editor.
  const sched = useMemo<Partial<ScenarioSchedule>>(() => {
    try {
      return JSON.parse(scheduleJson) as ScenarioSchedule;
    } catch {
      return {};
    }
  }, [scheduleJson]);

  const asg = useMemo<Partial<ScenarioProfileAssignment>>(() => {
    try {
      return JSON.parse(assignmentJson) as ScenarioProfileAssignment;
    } catch {
      return {};
    }
  }, [assignmentJson]);

  const patchSched = useCallback(
    (patch: Partial<ScenarioSchedule>) => {
      setScheduleJson(JSON.stringify({ ...sched, ...patch }, null, 2));
    },
    [sched],
  );

  const patchAsg = useCallback(
    (patch: Partial<ScenarioProfileAssignment>) => {
      setAssignmentJson(JSON.stringify({ ...asg, ...patch }, null, 2));
    },
    [asg],
  );

  const profileOptions = useMemo<Option[]>(
    () =>
      profiles.map((p) => ({ label: `${p.name} (${p.browser})`, value: p.id })),
    [profiles],
  );

  const selectedProfileOptions = useMemo<Option[]>(
    () =>
      (asg.profile_ids ?? []).map((id) => ({
        label: profileName(id),
        value: id,
      })),
    [asg.profile_ids, profileName],
  );

  const newSchedule = useCallback(() => {
    const tpl = newScheduleJson();
    const id = (JSON.parse(tpl) as ScenarioSchedule).id;
    setSelectedScheduleId(null);
    setScheduleJson(tpl);
    setAssignmentJson(assignmentJsonFor(id));
    setShowScheduleJson(false);
  }, []);

  const selectSchedule = useCallback(async (s: ScenarioSchedule) => {
    setSelectedScheduleId(s.id);
    setScheduleJson(JSON.stringify(s, null, 2));
    try {
      const a = await invoke<ScenarioProfileAssignment | null>(
        "scenario_get_assignment",
        { scheduleId: s.id },
      );
      setAssignmentJson(
        a ? JSON.stringify(a, null, 2) : assignmentJsonFor(s.id),
      );
    } catch {
      setAssignmentJson(assignmentJsonFor(s.id));
    }
  }, []);

  // Save schedule and its profile assignment together — the two were a single
  // logical unit but used to be two buttons, so the assignment was easy to skip.
  const handleSaveSchedule = useCallback(async () => {
    try {
      const schedule = JSON.parse(scheduleJson) as ScenarioSchedule;
      if (!schedule.id || !schedule.scenario_id) {
        showErrorToast(t("scenarios.errors.scheduleFields"));
        return;
      }
      const assignment = JSON.parse(
        assignmentJson,
      ) as ScenarioProfileAssignment;
      await invoke("scenario_save_schedule", { schedule });
      if (assignment.schedule_id) {
        await invoke("scenario_save_assignment", { assignment });
      }
      showSuccessToast(t("scenarios.scheduleSaved"));
      await loadSchedules();
    } catch (err) {
      showErrorToast(t("scenarios.errors.json", { error: String(err) }));
    }
  }, [scheduleJson, assignmentJson, loadSchedules, t]);

  const requestDeleteSchedule = useCallback(() => {
    if (!selectedScheduleId) return;
    setPendingDelete({
      kind: "schedule",
      id: selectedScheduleId,
      name: sched.name ?? selectedScheduleId,
    });
  }, [selectedScheduleId, sched.name]);

  // Runs the actual delete once the confirmation dialog is accepted.
  const confirmDelete = useCallback(async () => {
    if (!pendingDelete) return;
    setIsDeleting(true);
    try {
      if (pendingDelete.kind === "scenario") {
        await invoke("scenario_delete", { scenarioId: pendingDelete.id });
        const fresh = newScenario();
        setSelectedId(null);
        setEditorScenario(fresh);
        setEditorJson(JSON.stringify(fresh, null, 2));
        showSuccessToast(t("scenarios.deleted"));
        await loadScenarios();
      } else {
        await invoke("scenario_delete_schedule", {
          scheduleId: pendingDelete.id,
        });
        setSelectedScheduleId(null);
        setScheduleJson(newScheduleJson());
        setAssignmentJson(assignmentJsonFor(""));
        showSuccessToast(t("scenarios.scheduleDeleted"));
        await loadSchedules();
      }
    } catch (err) {
      showErrorToast(t("scenarios.errors.delete", { error: String(err) }));
    } finally {
      setIsDeleting(false);
      setPendingDelete(null);
    }
  }, [pendingDelete, loadScenarios, loadSchedules, t]);

  // ----- AI handlers -----
  const handleSaveAi = useCallback(async () => {
    try {
      await invoke("scenario_set_ai_config", {
        config: {
          provider: aiProvider,
          model: aiModel,
          api_key: aiApiKey, // rỗng = giữ key cũ
          base_url: aiBaseUrl || null,
          max_tokens: Number(aiMaxTokens) || 1024,
          temperature: Number(aiTemperature) || 0.3,
        },
      });
      setAiApiKey("");
      showSuccessToast(t("scenarios.aiSaved"));
      await loadAiConfig();
    } catch (err) {
      showErrorToast(t("scenarios.errors.save", { error: String(err) }));
    }
  }, [
    aiProvider,
    aiModel,
    aiApiKey,
    aiBaseUrl,
    aiMaxTokens,
    aiTemperature,
    loadAiConfig,
    t,
  ]);

  const handleClearAi = useCallback(async () => {
    try {
      await invoke("scenario_clear_ai_config");
      setAiApiKey("");
      setAiHasKey(false);
      showSuccessToast(t("scenarios.aiCleared"));
    } catch (err) {
      showErrorToast(t("scenarios.errors.delete", { error: String(err) }));
    }
  }, [t]);

  const triggerType = (sched.trigger_type ?? "interval") as ScenarioTriggerType;

  return (
    <Dialog
      open={isOpen}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
      subPage={subPage}
    >
      <DialogContent className="max-w-6xl max-h-[90vh] h-[88vh] my-4 flex flex-col">
        {!subPage && (
          <DialogHeader className="shrink-0">
            <DialogTitle>{t("scenarios.title")}</DialogTitle>
          </DialogHeader>
        )}

        <div className="overflow-hidden flex-1 min-h-0 flex flex-col">
          <AnimatedTabs
            defaultValue="editor"
            className="flex flex-col flex-1 min-h-0"
          >
            <AnimatedTabsList>
              <AnimatedTabsTrigger value="editor">
                {t("scenarios.tabEditor")}
              </AnimatedTabsTrigger>
              <AnimatedTabsTrigger value="runs">
                {t("scenarios.tabRuns")}
              </AnimatedTabsTrigger>
              <AnimatedTabsTrigger value="schedules">
                {t("scenarios.tabSchedules")}
              </AnimatedTabsTrigger>
              <AnimatedTabsTrigger value="ai">
                {t("scenarios.tabAi")}
              </AnimatedTabsTrigger>
            </AnimatedTabsList>

            {/* ---------- Editor ---------- */}
            <AnimatedTabsContent value="editor" className="mt-4 flex-1 min-h-0">
              <div className="flex gap-4 h-full min-h-0">
                {/* Scenario rail */}
                <aside className="w-56 shrink-0 flex flex-col gap-2 min-h-0">
                  <div className="flex items-center justify-between px-1">
                    <span className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                      {t("scenarios.form.scenariosHeading")}
                    </span>
                    {scenarios.length > 0 && (
                      <Badge variant="secondary" className="h-5 px-1.5">
                        {scenarios.length}
                      </Badge>
                    )}
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    className="justify-center"
                    onClick={handleNew}
                  >
                    <LuPlus className="size-3.5" /> {t("scenarios.new")}
                  </Button>
                  <div className="flex flex-col gap-1 overflow-y-auto min-h-0 flex-1 pr-1">
                    {scenarios.map((s) => {
                      const active = selectedId === s.id;
                      return (
                        <button
                          key={s.id}
                          type="button"
                          onClick={() => void selectScenario(s.id)}
                          className={`text-left rounded-md border px-2.5 py-2 transition-colors ${
                            active
                              ? "border-primary/40 bg-primary/10"
                              : "border-transparent hover:bg-accent/50"
                          }`}
                        >
                          <p className="text-sm font-medium truncate">
                            {s.name}
                          </p>
                          <p className="text-[11px] text-muted-foreground">
                            {t("scenarios.form.blocks", {
                              count: s.blocks?.length ?? 0,
                            })}
                          </p>
                        </button>
                      );
                    })}
                    {scenarios.length === 0 && (
                      <p className="text-xs text-muted-foreground px-2 py-3 text-center">
                        {t("scenarios.empty")}
                      </p>
                    )}
                  </div>
                </aside>

                {/* Editor panel */}
                <section className="flex-1 min-w-0 flex flex-col gap-3 min-h-0">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-sm font-medium truncate text-muted-foreground">
                      {editorMode === "visual"
                        ? editorScenario.name
                        : t("scenarios.builder.json")}
                    </span>
                    <div className="flex items-center gap-0.5 rounded-md border p-0.5 shrink-0">
                      {(["visual", "json"] as const).map((m) => (
                        <button
                          key={m}
                          type="button"
                          onClick={() => switchMode(m)}
                          className={`text-xs px-2.5 py-1 rounded transition-colors ${
                            editorMode === m
                              ? "bg-accent text-foreground"
                              : "text-muted-foreground hover:text-foreground"
                          }`}
                        >
                          {t(`scenarios.builder.${m}`)}
                        </button>
                      ))}
                    </div>
                  </div>

                  {editorMode === "json" ? (
                    <Textarea
                      value={editorJson}
                      onChange={(e) => setEditorJson(e.target.value)}
                      spellCheck={false}
                      className="font-mono text-xs flex-1 min-h-0 resize-none"
                    />
                  ) : (
                    <div className="flex flex-col gap-3 flex-1 min-h-0 overflow-y-auto pr-1">
                      <div className="grid grid-cols-2 gap-3">
                        <div className="flex flex-col gap-1">
                          <FieldLabel>{t("scenarios.builder.name")}</FieldLabel>
                          <Input
                            value={editorScenario.name}
                            onChange={(e) =>
                              setEditorScenario({
                                ...editorScenario,
                                name: e.target.value,
                              })
                            }
                            className="h-8 text-sm"
                          />
                        </div>
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.builder.description")}
                          </FieldLabel>
                          <Input
                            value={editorScenario.description ?? ""}
                            onChange={(e) =>
                              setEditorScenario({
                                ...editorScenario,
                                description: e.target.value,
                              })
                            }
                            className="h-8 text-sm"
                          />
                        </div>
                      </div>
                      <BlockEditor
                        blocks={editorScenario.blocks}
                        onChange={(blocks) =>
                          setEditorScenario({ ...editorScenario, blocks })
                        }
                      />
                    </div>
                  )}

                  {/* Action bar: save/delete on the left, run strip on the right */}
                  <div className="flex flex-wrap items-center gap-2 border-t pt-3">
                    <Button size="sm" onClick={() => void handleSaveScenario()}>
                      <LuSave className="size-3.5" /> {t("scenarios.save")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="text-destructive hover:text-destructive"
                      disabled={!selectedId}
                      onClick={requestDeleteScenario}
                    >
                      <LuTrash2 className="size-3.5" /> {t("scenarios.delete")}
                    </Button>
                    <div className="flex-1" />
                    <div className="flex items-center gap-2 rounded-md border bg-muted/30 p-1 pl-2">
                      <Select
                        value={runProfileId}
                        onValueChange={setRunProfileId}
                      >
                        <SelectTrigger className="w-52 h-8 border-0 bg-transparent shadow-none focus:ring-0">
                          <SelectValue
                            placeholder={t("scenarios.pickProfile")}
                          />
                        </SelectTrigger>
                        <SelectContent>
                          {runnableProfiles.map((p) => (
                            <SelectItem key={p.id} value={p.id}>
                              {p.name} ({p.browser})
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                      <Button
                        size="sm"
                        disabled={isRunning || runnableProfiles.length === 0}
                        onClick={() => void handleRun()}
                      >
                        <LuPlay className="size-3.5" /> {t("scenarios.run")}
                      </Button>
                    </div>
                  </div>
                  {runnableProfiles.length === 0 && (
                    <p className="text-xs text-muted-foreground -mt-1">
                      {t("scenarios.noRunningProfiles")}
                    </p>
                  )}
                </section>
              </div>
            </AnimatedTabsContent>

            {/* ---------- Runs ---------- */}
            <AnimatedTabsContent
              value="runs"
              className="mt-4 flex-1 min-h-0 flex flex-col gap-3"
            >
              {/* Active runs band */}
              {activeRuns.length > 0 && (
                <div className="flex flex-col gap-1.5 shrink-0">
                  {activeRuns.map((r) => (
                    <div
                      key={r.run_id}
                      className="flex items-center gap-2.5 text-sm rounded-md border border-success/30 bg-success/5 px-3 py-2"
                    >
                      <span className="size-2 rounded-full bg-success animate-pulse shrink-0" />
                      <span className="truncate flex-1">
                        <span className="font-medium">
                          {profileName(r.profile_id)}
                        </span>
                        <span className="text-muted-foreground">
                          {" · "}
                          {scenarioName(r.scenario_id)}
                        </span>
                      </span>
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => void handleCancelRun(r.run_id)}
                      >
                        <LuBan className="size-3.5" /> {t("scenarios.cancel")}
                      </Button>
                    </div>
                  ))}
                </div>
              )}

              {/* Master-detail: history rail + run detail */}
              <div className="flex gap-4 h-full min-h-0">
                <aside className="w-80 shrink-0 flex flex-col gap-2 min-h-0">
                  <div className="flex items-center justify-between px-1">
                    <span className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
                      {t("scenarios.history")}
                    </span>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="h-7 px-2"
                      onClick={() => void loadRuns()}
                    >
                      <LuRefreshCw className="size-3.5" />{" "}
                      {t("scenarios.refresh")}
                    </Button>
                  </div>
                  <div className="flex flex-col gap-1 overflow-y-auto min-h-0 flex-1 pr-1">
                    {runs.map((r) => {
                      const active = selectedRunId === r.id;
                      return (
                        <button
                          key={r.id}
                          type="button"
                          onClick={() => void openRunDetail(r.id)}
                          className={`text-left px-2.5 py-2 rounded-md flex flex-col gap-1 border transition-colors ${
                            active
                              ? "border-primary/40 bg-primary/10"
                              : "border-transparent hover:bg-accent/50"
                          }`}
                        >
                          <span className="flex items-center gap-2">
                            <span
                              className={`px-1.5 py-0.5 rounded text-[10px] font-medium shrink-0 ${statusTone(r.status)}`}
                            >
                              {t(`scenarios.statusLabels.${r.status}`)}
                            </span>
                            <span className="truncate flex-1 text-sm font-medium">
                              {profileName(r.profile_id)}
                            </span>
                          </span>
                          <span className="text-[11px] text-muted-foreground font-mono pl-0.5">
                            {r.steps_ok}✓ {r.steps_failed}✗ · {r.duration_ms}
                            ms
                          </span>
                        </button>
                      );
                    })}
                    {runs.length === 0 && (
                      <p className="text-xs text-muted-foreground px-2 py-3 text-center">
                        {t("scenarios.noRuns")}
                      </p>
                    )}
                  </div>
                </aside>

                <div className="flex-1 min-w-0 rounded-lg border bg-card overflow-y-auto">
                  {runDetail ? (
                    <div className="p-4">
                      <div className="flex items-center gap-2 mb-3">
                        <span
                          className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${statusTone(runDetail.status)}`}
                        >
                          {t(`scenarios.statusLabels.${runDetail.status}`)}
                        </span>
                        <span className="text-sm font-medium">
                          {scenarioName(runDetail.scenario_id)}
                        </span>
                        <span className="text-xs text-muted-foreground font-mono ml-auto">
                          {runDetail.duration_ms}ms
                        </span>
                      </div>
                      <div className="flex flex-col">
                        {runDetail.steps.map((s, i) => (
                          <div
                            key={`${s.block_id}-${i}`}
                            className="text-xs flex items-center gap-2 py-1.5 border-b border-border/50 last:border-0"
                          >
                            <span className="text-[10px] text-muted-foreground/60 font-mono w-5 shrink-0 text-right">
                              {i + 1}
                            </span>
                            <span
                              className={`size-1.5 rounded-full shrink-0 ${
                                s.status === "ok"
                                  ? "bg-success"
                                  : s.status === "failed"
                                    ? "bg-destructive"
                                    : "bg-muted-foreground/40"
                              }`}
                            />
                            <span className="font-mono truncate">
                              {s.block_type}
                            </span>
                            <span className="text-muted-foreground font-mono ml-auto shrink-0">
                              {s.duration_ms}ms
                            </span>
                            {s.error && (
                              <span className="text-destructive truncate basis-full pl-9">
                                {s.error}
                              </span>
                            )}
                          </div>
                        ))}
                      </div>
                      {runDetail.warnings.length > 0 && (
                        <p className="text-[11px] text-warning mt-3">
                          {runDetail.warnings.join("; ")}
                        </p>
                      )}
                    </div>
                  ) : (
                    <div className="h-full grid place-items-center p-8">
                      <p className="text-sm text-muted-foreground text-center">
                        {t("scenarios.form.selectRun")}
                      </p>
                    </div>
                  )}
                </div>
              </div>
            </AnimatedTabsContent>

            {/* ---------- Schedules ---------- */}
            <AnimatedTabsContent
              value="schedules"
              className="mt-4 flex-1 min-h-0"
            >
              <div className="flex gap-4 h-full min-h-0">
                {/* Schedule rail */}
                <aside className="w-56 shrink-0 flex flex-col gap-2 min-h-0">
                  <span className="text-xs font-semibold uppercase tracking-wide text-muted-foreground px-1">
                    {t("scenarios.form.schedulesHeading")}
                  </span>
                  <Button
                    variant="outline"
                    size="sm"
                    className="justify-center"
                    onClick={newSchedule}
                  >
                    <LuPlus className="size-3.5" /> {t("scenarios.new")}
                  </Button>
                  <div className="flex flex-col gap-1 overflow-y-auto min-h-0 flex-1 pr-1">
                    {schedules.map((s) => {
                      const active = selectedScheduleId === s.id;
                      return (
                        <button
                          key={s.id}
                          type="button"
                          onClick={() => void selectSchedule(s)}
                          className={`text-left rounded-md border px-2.5 py-2 flex items-center gap-2 transition-colors ${
                            active
                              ? "border-primary/40 bg-primary/10"
                              : "border-transparent hover:bg-accent/50"
                          }`}
                        >
                          <span
                            className={`size-2 rounded-full shrink-0 ${
                              s.enabled
                                ? "bg-success"
                                : "bg-muted-foreground/40"
                            }`}
                          />
                          <span className="text-sm truncate">{s.name}</span>
                        </button>
                      );
                    })}
                    {schedules.length === 0 && (
                      <p className="text-xs text-muted-foreground px-2 py-3 text-center">
                        {t("scenarios.empty")}
                      </p>
                    )}
                  </div>
                </aside>

                {/* Schedule form */}
                <section className="flex-1 min-w-0 flex flex-col gap-3 min-h-0">
                  <div className="flex-1 min-h-0 overflow-y-auto pr-1 flex flex-col gap-4">
                    {/* Schedule fields */}
                    <div className="rounded-lg border bg-card p-4 flex flex-col gap-3">
                      <div className="flex items-center justify-between gap-3">
                        <Input
                          value={sched.name ?? ""}
                          onChange={(e) => patchSched({ name: e.target.value })}
                          placeholder={t("scenarios.form.scheduleName")}
                          className="h-8 text-sm font-medium max-w-xs"
                        />
                        <span className="flex items-center gap-2 text-xs shrink-0">
                          {t("scenarios.form.enabled")}
                          <AnimatedSwitch
                            checked={sched.enabled ?? false}
                            onCheckedChange={(c) => patchSched({ enabled: c })}
                          />
                        </span>
                      </div>

                      <div className="grid grid-cols-2 gap-3">
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.scenario")}
                          </FieldLabel>
                          <Select
                            value={sched.scenario_id || ""}
                            onValueChange={(v) =>
                              patchSched({ scenario_id: v })
                            }
                          >
                            <SelectTrigger className="h-8">
                              <SelectValue
                                placeholder={
                                  scenarios.length === 0
                                    ? t("scenarios.form.noScenarios")
                                    : t("scenarios.form.pickScenario")
                                }
                              />
                            </SelectTrigger>
                            <SelectContent>
                              {scenarios.map((s) => (
                                <SelectItem key={s.id} value={s.id}>
                                  {s.name}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>
                        <div className="flex flex-col gap-1">
                          <FieldLabel>{t("scenarios.form.trigger")}</FieldLabel>
                          <Select
                            value={triggerType}
                            onValueChange={(v) =>
                              patchSched({
                                trigger_type: v as ScenarioTriggerType,
                              })
                            }
                          >
                            <SelectTrigger className="h-8">
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              {TRIGGER_TYPES.map((tt) => (
                                <SelectItem key={tt} value={tt}>
                                  {t(`scenarios.form.triggerOpts.${tt}`)}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>
                      </div>

                      {triggerType === "interval" && (
                        <div className="flex flex-col gap-1 max-w-[12rem]">
                          <FieldLabel>
                            {t("scenarios.form.intervalMinutes")}
                          </FieldLabel>
                          <Input
                            type="number"
                            min={1}
                            value={sched.interval_minutes ?? ""}
                            onChange={(e) =>
                              patchSched({
                                interval_minutes:
                                  e.target.value === ""
                                    ? undefined
                                    : Number(e.target.value),
                              })
                            }
                            className="h-8"
                          />
                        </div>
                      )}
                      {triggerType === "cron" && (
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.cronExpr")}
                          </FieldLabel>
                          <Input
                            value={sched.cron_expr ?? ""}
                            onChange={(e) =>
                              patchSched({
                                cron_expr: e.target.value || undefined,
                              })
                            }
                            placeholder="0 */6 * * *"
                            className="h-8 font-mono text-xs"
                          />
                        </div>
                      )}

                      <div className="grid grid-cols-3 gap-3">
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.windowStart")}
                          </FieldLabel>
                          <Input
                            value={sched.time_window_start ?? ""}
                            onChange={(e) =>
                              patchSched({
                                time_window_start: e.target.value || undefined,
                              })
                            }
                            placeholder={t("scenarios.form.windowAny")}
                            className="h-8"
                          />
                        </div>
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.windowEnd")}
                          </FieldLabel>
                          <Input
                            value={sched.time_window_end ?? ""}
                            onChange={(e) =>
                              patchSched({
                                time_window_end: e.target.value || undefined,
                              })
                            }
                            placeholder={t("scenarios.form.windowAny")}
                            className="h-8"
                          />
                        </div>
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.maxRunsPerDay")}
                          </FieldLabel>
                          <Input
                            type="number"
                            min={1}
                            value={sched.max_runs_per_day ?? ""}
                            onChange={(e) =>
                              patchSched({
                                max_runs_per_day:
                                  e.target.value === ""
                                    ? undefined
                                    : Number(e.target.value),
                              })
                            }
                            placeholder={t("scenarios.form.unlimited")}
                            className="h-8"
                          />
                        </div>
                      </div>
                    </div>

                    {/* Assignment fields */}
                    <div className="rounded-lg border bg-card p-4 flex flex-col gap-3">
                      <span className="text-sm font-medium">
                        {t("scenarios.form.assignment")}
                      </span>
                      <div className="flex flex-col gap-1">
                        <FieldLabel>{t("scenarios.form.profiles")}</FieldLabel>
                        <MultipleSelector
                          value={selectedProfileOptions}
                          defaultOptions={profileOptions}
                          options={profileOptions}
                          placeholder={t("scenarios.form.pickProfiles")}
                          hidePlaceholderWhenSelected
                          onChange={(opts) =>
                            patchAsg({
                              profile_ids: opts.map((o) => o.value),
                            })
                          }
                          emptyIndicator={
                            <p className="text-center text-xs text-muted-foreground py-2">
                              {t("scenarios.empty")}
                            </p>
                          }
                        />
                      </div>
                      <div className="grid grid-cols-3 gap-3">
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.rotation")}
                          </FieldLabel>
                          <Select
                            value={asg.rotation_mode ?? "round_robin"}
                            onValueChange={(v) =>
                              patchAsg({
                                rotation_mode: v as ScenarioRotationMode,
                              })
                            }
                          >
                            <SelectTrigger className="h-8">
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              {ROTATION_MODES.map((m) => (
                                <SelectItem key={m} value={m}>
                                  {t(`scenarios.form.rotationOpts.${m}`)}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </div>
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.maxParallel")}
                          </FieldLabel>
                          <Input
                            type="number"
                            min={1}
                            value={asg.max_parallel ?? 1}
                            onChange={(e) =>
                              patchAsg({
                                max_parallel: Number(e.target.value) || 1,
                              })
                            }
                            className="h-8"
                          />
                        </div>
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.cooldownMinutes")}
                          </FieldLabel>
                          <Input
                            type="number"
                            min={0}
                            value={asg.cooldown_minutes ?? 0}
                            onChange={(e) =>
                              patchAsg({
                                cooldown_minutes: Number(e.target.value) || 0,
                              })
                            }
                            className="h-8"
                          />
                        </div>
                      </div>
                      <span className="flex items-center gap-2 text-xs">
                        <AnimatedSwitch
                          checked={asg.run_headless ?? false}
                          onCheckedChange={(c) => patchAsg({ run_headless: c })}
                        />
                        {t("scenarios.form.headless")}
                      </span>
                    </div>

                    {/* Advanced JSON escape hatch */}
                    <div className="rounded-lg border bg-card">
                      <button
                        type="button"
                        onClick={() => setShowScheduleJson((v) => !v)}
                        className="w-full flex items-center gap-2 px-4 py-2.5 text-xs font-medium text-muted-foreground hover:text-foreground transition-colors"
                      >
                        {showScheduleJson ? (
                          <LuChevronDown className="size-3.5" />
                        ) : (
                          <LuChevronRight className="size-3.5" />
                        )}
                        <LuCode className="size-3.5" />
                        {t("scenarios.form.advancedJson")}
                      </button>
                      {showScheduleJson && (
                        <div className="grid grid-cols-2 gap-3 p-4 pt-0">
                          <div className="flex flex-col gap-1">
                            <FieldLabel>{t("scenarios.schedule")}</FieldLabel>
                            <Textarea
                              value={scheduleJson}
                              onChange={(e) => setScheduleJson(e.target.value)}
                              spellCheck={false}
                              className="font-mono text-xs h-56 resize-none"
                            />
                          </div>
                          <div className="flex flex-col gap-1">
                            <FieldLabel>{t("scenarios.assignment")}</FieldLabel>
                            <Textarea
                              value={assignmentJson}
                              onChange={(e) =>
                                setAssignmentJson(e.target.value)
                              }
                              spellCheck={false}
                              className="font-mono text-xs h-56 resize-none"
                            />
                          </div>
                        </div>
                      )}
                    </div>
                  </div>

                  {/* Action bar — one Save persists both schedule + assignment */}
                  <div className="flex flex-wrap items-center gap-2 border-t pt-3">
                    <Button size="sm" onClick={() => void handleSaveSchedule()}>
                      <LuSave className="size-3.5" />{" "}
                      {t("scenarios.saveSchedule")}
                    </Button>
                    <div className="flex-1" />
                    <Button
                      size="sm"
                      variant="ghost"
                      className="text-destructive hover:text-destructive"
                      disabled={!selectedScheduleId}
                      onClick={requestDeleteSchedule}
                    >
                      <LuTrash2 className="size-3.5" /> {t("scenarios.delete")}
                    </Button>
                  </div>
                  <p className="text-xs text-muted-foreground -mt-1">
                    {t("scenarios.scheduleHint")}
                  </p>
                </section>
              </div>
            </AnimatedTabsContent>

            {/* ---------- AI ---------- */}
            <AnimatedTabsContent
              value="ai"
              className="mt-4 flex-1 min-h-0 overflow-y-auto"
            >
              <div className="max-w-2xl mx-auto flex flex-col gap-4">
                <div className="flex items-start justify-between gap-3">
                  <div className="flex flex-col gap-1">
                    <h3 className="text-base font-semibold">
                      {t("scenarios.tabAi")}
                    </h3>
                    <p className="text-xs text-muted-foreground max-w-prose">
                      {t("scenarios.aiHint")}
                    </p>
                  </div>
                  {aiHasKey && (
                    <Badge
                      variant="secondary"
                      className="bg-success/15 text-success shrink-0"
                    >
                      {t("scenarios.form.keySet")}
                    </Badge>
                  )}
                </div>
                <div className="rounded-lg border bg-card p-5 flex flex-col gap-4">
                  <div className="grid grid-cols-2 gap-3">
                    <div className="flex flex-col gap-1">
                      <FieldLabel>{t("scenarios.aiProvider")}</FieldLabel>
                      <Select
                        value={aiProvider}
                        onValueChange={(v) =>
                          setAiProvider(v as ScenarioAiProvider)
                        }
                      >
                        <SelectTrigger className="h-8">
                          <SelectValue />
                        </SelectTrigger>
                        <SelectContent>
                          {PROVIDERS.map((p) => (
                            <SelectItem key={p} value={p}>
                              {p}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                    <div className="flex flex-col gap-1">
                      <FieldLabel>{t("scenarios.aiModel")}</FieldLabel>
                      <Input
                        value={aiModel}
                        onChange={(e) => setAiModel(e.target.value)}
                        className="h-8"
                      />
                    </div>
                  </div>
                  <div className="flex flex-col gap-1">
                    <FieldLabel>{t("scenarios.aiApiKey")}</FieldLabel>
                    <Input
                      type="password"
                      value={aiApiKey}
                      onChange={(e) => setAiApiKey(e.target.value)}
                      placeholder={
                        aiHasKey
                          ? t("scenarios.aiKeySet")
                          : t("scenarios.aiKeyEmpty")
                      }
                      className="h-8"
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <FieldLabel>{t("scenarios.aiBaseUrl")}</FieldLabel>
                    <Input
                      value={aiBaseUrl}
                      onChange={(e) => setAiBaseUrl(e.target.value)}
                      placeholder="https://..."
                      className="h-8"
                    />
                  </div>
                  <div className="grid grid-cols-2 gap-3">
                    <div className="flex flex-col gap-1">
                      <FieldLabel>{t("scenarios.aiMaxTokens")}</FieldLabel>
                      <Input
                        type="number"
                        value={aiMaxTokens}
                        onChange={(e) => setAiMaxTokens(e.target.value)}
                        className="h-8"
                      />
                    </div>
                    <div className="flex flex-col gap-1">
                      <FieldLabel>{t("scenarios.aiTemperature")}</FieldLabel>
                      <Input
                        type="number"
                        step="0.1"
                        value={aiTemperature}
                        onChange={(e) => setAiTemperature(e.target.value)}
                        className="h-8"
                      />
                    </div>
                  </div>
                  <div className="flex gap-2 border-t pt-3">
                    <Button size="sm" onClick={() => void handleSaveAi()}>
                      <LuSave className="size-3.5" /> {t("scenarios.save")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      className="text-destructive hover:text-destructive"
                      onClick={() => void handleClearAi()}
                    >
                      <LuTrash2 className="size-3.5" /> {t("scenarios.aiClear")}
                    </Button>
                  </div>
                </div>
              </div>
            </AnimatedTabsContent>
          </AnimatedTabs>
        </div>
        <DeleteConfirmationDialog
          isOpen={pendingDelete !== null}
          onClose={() => setPendingDelete(null)}
          onConfirm={confirmDelete}
          isLoading={isDeleting}
          title={
            pendingDelete?.kind === "schedule"
              ? t("scenarios.confirmDelete.scheduleTitle")
              : t("scenarios.confirmDelete.scenarioTitle")
          }
          description={
            pendingDelete?.kind === "schedule"
              ? t("scenarios.confirmDelete.scheduleDesc", {
                  name: pendingDelete?.name ?? "",
                })
              : t("scenarios.confirmDelete.scenarioDesc", {
                  name: pendingDelete?.name ?? "",
                })
          }
        />
      </DialogContent>
    </Dialog>
  );
}
