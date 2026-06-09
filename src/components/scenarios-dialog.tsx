"use client";

import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  LuBan,
  LuPlay,
  LuPlus,
  LuRefreshCw,
  LuSave,
  LuTrash2,
} from "react-icons/lu";
import { BlockEditor } from "@/components/block-editor";
import {
  AnimatedTabs,
  AnimatedTabsContent,
  AnimatedTabsList,
  AnimatedTabsTrigger,
} from "@/components/ui/animated-tabs";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
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
  ScenarioRunDetail,
  ScenarioRunInfo,
  ScenarioRunSummary,
  ScenarioSchedule,
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

  // ----- Runs tab -----
  const [runs, setRuns] = useState<ScenarioRunSummary[]>([]);
  const [activeRuns, setActiveRuns] = useState<ScenarioRunInfo[]>([]);
  const [runDetail, setRunDetail] = useState<ScenarioRunDetail | null>(null);

  // ----- Schedules tab -----
  const [schedules, setSchedules] = useState<ScenarioSchedule[]>([]);
  const [scheduleJson, setScheduleJson] = useState<string>(newScheduleJson());
  const [assignmentJson, setAssignmentJson] = useState<string>(
    assignmentJsonFor(""),
  );

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
  useEffect(() => {
    if (!isOpen) return;
    const id = setInterval(() => {
      void invoke<ScenarioRunInfo[]>("scenario_active_runs")
        .then(setActiveRuns)
        .catch(() => {});
    }, 3000);
    return () => clearInterval(id);
  }, [isOpen]);

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

  const handleDeleteScenario = useCallback(async () => {
    const scenario = getScenario();
    if (!scenario) return;
    try {
      await invoke("scenario_delete", { scenarioId: scenario.id });
      setSelectedId(null);
      setEditorJson(newScenarioJson());
      showSuccessToast(t("scenarios.deleted"));
      await loadScenarios();
    } catch (err) {
      showErrorToast(t("scenarios.errors.delete", { error: String(err) }));
    }
  }, [getScenario, loadScenarios, t]);

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
  const selectSchedule = useCallback(async (s: ScenarioSchedule) => {
    setScheduleJson(JSON.stringify(s, null, 2));
    try {
      const asg = await invoke<ScenarioProfileAssignment | null>(
        "scenario_get_assignment",
        { scheduleId: s.id },
      );
      setAssignmentJson(
        asg ? JSON.stringify(asg, null, 2) : assignmentJsonFor(s.id),
      );
    } catch {
      setAssignmentJson(assignmentJsonFor(s.id));
    }
  }, []);

  const handleSaveSchedule = useCallback(async () => {
    try {
      const schedule = JSON.parse(scheduleJson) as ScenarioSchedule;
      if (!schedule.id || !schedule.scenario_id) {
        showErrorToast(t("scenarios.errors.scheduleFields"));
        return;
      }
      await invoke("scenario_save_schedule", { schedule });
      showSuccessToast(t("scenarios.scheduleSaved"));
      await loadSchedules();
    } catch (err) {
      showErrorToast(t("scenarios.errors.json", { error: String(err) }));
    }
  }, [scheduleJson, loadSchedules, t]);

  const handleSaveAssignment = useCallback(async () => {
    try {
      const assignment = JSON.parse(
        assignmentJson,
      ) as ScenarioProfileAssignment;
      if (!assignment.schedule_id) {
        showErrorToast(t("scenarios.errors.scheduleFields"));
        return;
      }
      await invoke("scenario_save_assignment", { assignment });
      showSuccessToast(t("scenarios.assignmentSaved"));
    } catch (err) {
      showErrorToast(t("scenarios.errors.json", { error: String(err) }));
    }
  }, [assignmentJson, t]);

  const handleDeleteSchedule = useCallback(async () => {
    try {
      const schedule = JSON.parse(scheduleJson) as ScenarioSchedule;
      if (!schedule.id) return;
      await invoke("scenario_delete_schedule", { scheduleId: schedule.id });
      setScheduleJson(newScheduleJson());
      setAssignmentJson(assignmentJsonFor(""));
      showSuccessToast(t("scenarios.scheduleDeleted"));
      await loadSchedules();
    } catch (err) {
      showErrorToast(t("scenarios.errors.delete", { error: String(err) }));
    }
  }, [scheduleJson, loadSchedules, t]);

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

  return (
    <Dialog
      open={isOpen}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
      subPage={subPage}
    >
      <DialogContent className="max-w-4xl max-h-[85vh] my-8 flex flex-col">
        {!subPage && (
          <DialogHeader className="shrink-0">
            <DialogTitle>{t("scenarios.title")}</DialogTitle>
          </DialogHeader>
        )}

        <div className="overflow-y-auto flex-1 min-h-0">
          <AnimatedTabs defaultValue="editor">
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
            <AnimatedTabsContent value="editor" className="mt-4">
              <div className="flex gap-3">
                <div className="w-48 shrink-0 flex flex-col gap-1">
                  <Button
                    variant="outline"
                    size="sm"
                    className="justify-start"
                    onClick={handleNew}
                  >
                    <LuPlus className="size-3.5" /> {t("scenarios.new")}
                  </Button>
                  <div className="mt-1 flex flex-col gap-0.5 overflow-y-auto max-h-[50vh]">
                    {scenarios.map((s) => (
                      <button
                        key={s.id}
                        type="button"
                        onClick={() => void selectScenario(s.id)}
                        className={`text-left text-sm px-2 py-1.5 rounded-md truncate transition-colors ${
                          selectedId === s.id
                            ? "bg-accent text-foreground"
                            : "text-muted-foreground hover:bg-accent/50"
                        }`}
                      >
                        {s.name}
                      </button>
                    ))}
                    {scenarios.length === 0 && (
                      <p className="text-xs text-muted-foreground px-2 py-1">
                        {t("scenarios.empty")}
                      </p>
                    )}
                  </div>
                </div>

                <div className="flex-1 min-w-0 flex flex-col gap-2">
                  <div className="flex items-center gap-1 self-start rounded-md border p-0.5">
                    <button
                      type="button"
                      onClick={() => switchMode("visual")}
                      className={`text-xs px-2 py-0.5 rounded ${
                        editorMode === "visual"
                          ? "bg-accent text-foreground"
                          : "text-muted-foreground"
                      }`}
                    >
                      {t("scenarios.builder.visual")}
                    </button>
                    <button
                      type="button"
                      onClick={() => switchMode("json")}
                      className={`text-xs px-2 py-0.5 rounded ${
                        editorMode === "json"
                          ? "bg-accent text-foreground"
                          : "text-muted-foreground"
                      }`}
                    >
                      {t("scenarios.builder.json")}
                    </button>
                  </div>

                  {editorMode === "json" ? (
                    <Textarea
                      value={editorJson}
                      onChange={(e) => setEditorJson(e.target.value)}
                      spellCheck={false}
                      className="font-mono text-xs h-[46vh] resize-none"
                    />
                  ) : (
                    <div className="flex flex-col gap-2 h-[46vh] overflow-y-auto pr-1">
                      <div className="grid grid-cols-2 gap-2">
                        <div className="flex flex-col gap-0.5">
                          <span className="text-[11px] text-muted-foreground">
                            {t("scenarios.builder.name")}
                          </span>
                          <Input
                            value={editorScenario.name}
                            onChange={(e) =>
                              setEditorScenario({
                                ...editorScenario,
                                name: e.target.value,
                              })
                            }
                            className="h-7 text-xs"
                          />
                        </div>
                        <div className="flex flex-col gap-0.5">
                          <span className="text-[11px] text-muted-foreground">
                            {t("scenarios.builder.description")}
                          </span>
                          <Input
                            value={editorScenario.description ?? ""}
                            onChange={(e) =>
                              setEditorScenario({
                                ...editorScenario,
                                description: e.target.value,
                              })
                            }
                            className="h-7 text-xs"
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
                  <div className="flex flex-wrap items-center gap-2">
                    <Button size="sm" onClick={() => void handleSaveScenario()}>
                      <LuSave className="size-3.5" /> {t("scenarios.save")}
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => void handleDeleteScenario()}
                    >
                      <LuTrash2 className="size-3.5" /> {t("scenarios.delete")}
                    </Button>
                    <div className="flex-1" />
                    <Select
                      value={runProfileId}
                      onValueChange={setRunProfileId}
                    >
                      <SelectTrigger className="w-52 h-8">
                        <SelectValue placeholder={t("scenarios.pickProfile")} />
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
                  {runnableProfiles.length === 0 && (
                    <p className="text-xs text-muted-foreground">
                      {t("scenarios.noRunningProfiles")}
                    </p>
                  )}
                </div>
              </div>
            </AnimatedTabsContent>

            {/* ---------- Runs ---------- */}
            <AnimatedTabsContent
              value="runs"
              className="mt-4 flex flex-col gap-3"
            >
              <div className="flex items-center justify-between">
                <Label className="text-sm font-medium">
                  {t("scenarios.activeRuns")}
                </Label>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => void loadRuns()}
                >
                  <LuRefreshCw className="size-3.5" /> {t("scenarios.refresh")}
                </Button>
              </div>
              {activeRuns.length === 0 ? (
                <p className="text-xs text-muted-foreground">
                  {t("scenarios.noActive")}
                </p>
              ) : (
                <div className="flex flex-col gap-1">
                  {activeRuns.map((r) => (
                    <div
                      key={r.run_id}
                      className="flex items-center gap-2 text-sm rounded-md border bg-card px-2 py-1.5"
                    >
                      <span className="size-2 rounded-full bg-green-500 animate-pulse" />
                      <span className="truncate flex-1">
                        {profileName(r.profile_id)} · {r.scenario_id}
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

              <Label className="text-sm font-medium mt-2">
                {t("scenarios.history")}
              </Label>
              <div className="flex gap-3">
                <div className="flex-1 min-w-0 flex flex-col gap-0.5 max-h-[40vh] overflow-y-auto">
                  {runs.map((r) => (
                    <button
                      key={r.id}
                      type="button"
                      onClick={() => void openRunDetail(r.id)}
                      className="text-left text-xs px-2 py-1.5 rounded-md hover:bg-accent/50 flex items-center gap-2"
                    >
                      <span
                        className={`px-1.5 py-0.5 rounded text-[10px] ${
                          r.status === "success"
                            ? "bg-green-500/15 text-green-600"
                            : r.status === "failed"
                              ? "bg-red-500/15 text-red-600"
                              : "bg-amber-500/15 text-amber-600"
                        }`}
                      >
                        {r.status}
                      </span>
                      <span className="truncate flex-1">
                        {profileName(r.profile_id)}
                      </span>
                      <span className="text-muted-foreground shrink-0">
                        {r.steps_ok}✓ {r.steps_failed}✗ · {r.duration_ms}ms
                      </span>
                    </button>
                  ))}
                  {runs.length === 0 && (
                    <p className="text-xs text-muted-foreground px-2 py-1">
                      {t("scenarios.noRuns")}
                    </p>
                  )}
                </div>
                {runDetail && (
                  <div className="w-72 shrink-0 rounded-md border bg-card p-2 max-h-[40vh] overflow-y-auto">
                    <p className="text-xs font-medium mb-1">
                      {runDetail.status} · {runDetail.duration_ms}ms
                    </p>
                    <div className="flex flex-col gap-0.5">
                      {runDetail.steps.map((s, i) => (
                        <div
                          key={`${s.block_id}-${i}`}
                          className="text-[11px] flex items-center gap-1.5"
                        >
                          <span className="text-muted-foreground">
                            {s.status}
                          </span>
                          <span className="truncate">{s.block_type}</span>
                          {s.error && (
                            <span className="text-red-500 truncate">
                              {s.error}
                            </span>
                          )}
                        </div>
                      ))}
                    </div>
                    {runDetail.warnings.length > 0 && (
                      <p className="text-[11px] text-amber-600 mt-1">
                        {runDetail.warnings.join("; ")}
                      </p>
                    )}
                  </div>
                )}
              </div>
            </AnimatedTabsContent>

            {/* ---------- Schedules ---------- */}
            <AnimatedTabsContent
              value="schedules"
              className="mt-4 flex flex-col gap-2"
            >
              <div className="flex gap-3">
                <div className="w-48 shrink-0 flex flex-col gap-1">
                  <Button
                    variant="outline"
                    size="sm"
                    className="justify-start"
                    onClick={() => {
                      const tpl = newScheduleJson();
                      setScheduleJson(tpl);
                      setAssignmentJson(
                        assignmentJsonFor(
                          (JSON.parse(tpl) as ScenarioSchedule).id,
                        ),
                      );
                    }}
                  >
                    <LuPlus className="size-3.5" /> {t("scenarios.new")}
                  </Button>
                  <div className="mt-1 flex flex-col gap-0.5 overflow-y-auto max-h-[50vh]">
                    {schedules.map((s) => (
                      <button
                        key={s.id}
                        type="button"
                        onClick={() => void selectSchedule(s)}
                        className="text-left text-sm px-2 py-1.5 rounded-md truncate text-muted-foreground hover:bg-accent/50 flex items-center gap-1.5"
                      >
                        <span
                          className={`size-2 rounded-full shrink-0 ${
                            s.enabled
                              ? "bg-green-500"
                              : "bg-muted-foreground/40"
                          }`}
                        />
                        {s.name}
                      </button>
                    ))}
                    {schedules.length === 0 && (
                      <p className="text-xs text-muted-foreground px-2 py-1">
                        {t("scenarios.empty")}
                      </p>
                    )}
                  </div>
                </div>
                <div className="flex-1 min-w-0 grid grid-cols-2 gap-2">
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">{t("scenarios.schedule")}</Label>
                    <Textarea
                      value={scheduleJson}
                      onChange={(e) => setScheduleJson(e.target.value)}
                      spellCheck={false}
                      className="font-mono text-xs h-[38vh] resize-none"
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">
                      {t("scenarios.assignment")}
                    </Label>
                    <Textarea
                      value={assignmentJson}
                      onChange={(e) => setAssignmentJson(e.target.value)}
                      spellCheck={false}
                      className="font-mono text-xs h-[38vh] resize-none"
                    />
                  </div>
                </div>
              </div>
              <div className="flex flex-wrap gap-2">
                <Button size="sm" onClick={() => void handleSaveSchedule()}>
                  <LuSave className="size-3.5" /> {t("scenarios.saveSchedule")}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void handleSaveAssignment()}
                >
                  <LuSave className="size-3.5" />{" "}
                  {t("scenarios.saveAssignment")}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void handleDeleteSchedule()}
                >
                  <LuTrash2 className="size-3.5" /> {t("scenarios.delete")}
                </Button>
              </div>
              <p className="text-xs text-muted-foreground">
                {t("scenarios.scheduleHint")}
              </p>
            </AnimatedTabsContent>

            {/* ---------- AI ---------- */}
            <AnimatedTabsContent
              value="ai"
              className="mt-4 flex flex-col gap-3"
            >
              <div className="rounded-md border bg-card p-4 flex flex-col gap-3">
                <div className="grid grid-cols-2 gap-3">
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">
                      {t("scenarios.aiProvider")}
                    </Label>
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
                    <Label className="text-xs">{t("scenarios.aiModel")}</Label>
                    <Input
                      value={aiModel}
                      onChange={(e) => setAiModel(e.target.value)}
                      className="h-8"
                    />
                  </div>
                </div>
                <div className="flex flex-col gap-1">
                  <Label className="text-xs">{t("scenarios.aiApiKey")}</Label>
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
                  <Label className="text-xs">{t("scenarios.aiBaseUrl")}</Label>
                  <Input
                    value={aiBaseUrl}
                    onChange={(e) => setAiBaseUrl(e.target.value)}
                    placeholder="https://..."
                    className="h-8"
                  />
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">
                      {t("scenarios.aiMaxTokens")}
                    </Label>
                    <Input
                      type="number"
                      value={aiMaxTokens}
                      onChange={(e) => setAiMaxTokens(e.target.value)}
                      className="h-8"
                    />
                  </div>
                  <div className="flex flex-col gap-1">
                    <Label className="text-xs">
                      {t("scenarios.aiTemperature")}
                    </Label>
                    <Input
                      type="number"
                      step="0.1"
                      value={aiTemperature}
                      onChange={(e) => setAiTemperature(e.target.value)}
                      className="h-8"
                    />
                  </div>
                </div>
                <div className="flex gap-2">
                  <Button size="sm" onClick={() => void handleSaveAi()}>
                    <LuSave className="size-3.5" /> {t("scenarios.save")}
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void handleClearAi()}
                  >
                    <LuTrash2 className="size-3.5" /> {t("scenarios.aiClear")}
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground">
                  {t("scenarios.aiHint")}
                </p>
              </div>
            </AnimatedTabsContent>
          </AnimatedTabs>
        </div>
      </DialogContent>
    </Dialog>
  );
}
