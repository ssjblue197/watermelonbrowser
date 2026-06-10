"use client";

import {
  type ColumnDef,
  flexRender,
  getCoreRowModel,
  type Row,
  type RowSelectionState,
  useReactTable,
} from "@tanstack/react-table";
import { invoke } from "@tauri-apps/api/core";
import {
  Fragment,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import {
  LuBan,
  LuBookOpen,
  LuChevronDown,
  LuChevronRight,
  LuCircleCheck,
  LuClock,
  LuCode,
  LuEye,
  LuGlobe,
  LuPencil,
  LuPlay,
  LuPlug,
  LuPlus,
  LuRefreshCw,
  LuSave,
  LuTrash2,
} from "react-icons/lu";
import {
  BlockEditor,
  hasChildren,
  prettify,
  summarize,
} from "@/components/block-editor";
import {
  DataTableActionBar,
  DataTableActionBarAction,
  DataTableActionBarSelection,
} from "@/components/data-table-action-bar";
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
import { Checkbox } from "@/components/ui/checkbox";
import { CopyToClipboard } from "@/components/ui/copy-to-clipboard";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { FadingScrollArea } from "@/components/ui/fading-scroll-area";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Textarea } from "@/components/ui/textarea";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { showErrorToast, showSuccessToast } from "@/lib/toast-utils";
import { cn } from "@/lib/utils";
import type {
  BrowserProfile,
  GroupWithCount,
  Scenario,
  ScenarioAiConfigView,
  ScenarioAiProvider,
  ScenarioBlock,
  ScenarioOnError,
  ScenarioProfileAssignment,
  ScenarioRotationMode,
  ScenarioRunCaps,
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
      scenario_ids: [],
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

/** Per-provider suggestions: model presets (typeable), default endpoint, key need. */
const PROVIDER_META: Record<
  ScenarioAiProvider,
  { models: string[]; baseUrl: string; needsKey: boolean }
> = {
  anthropic: {
    models: [
      "claude-opus-4-8",
      "claude-sonnet-4-6",
      "claude-haiku-4-5",
      "claude-fable-5",
    ],
    baseUrl: "https://api.anthropic.com",
    needsKey: true,
  },
  openai: {
    models: ["gpt-4o", "gpt-4o-mini", "gpt-4.1", "o3-mini"],
    baseUrl: "https://api.openai.com/v1",
    needsKey: true,
  },
  gemini: {
    models: ["gemini-2.0-flash", "gemini-1.5-pro", "gemini-1.5-flash"],
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    needsKey: true,
  },
  ollama: {
    models: ["llama3.1", "qwen2.5", "mistral"],
    baseUrl: "http://127.0.0.1:11434/v1",
    needsKey: false,
  },
};

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

const ON_ERROR_MODES: ScenarioOnError[] = ["stop", "skip", "retry"];

const DEFAULT_CAPS: ScenarioRunCaps = {
  max_steps: 2000,
  max_loop_iterations: 1000,
  max_total_secs: 3600,
  max_ai_tokens: 200_000,
};

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

/** Compact duration: ms under 1s, else seconds with one decimal. */
function formatDuration(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
}

/** Locale-aware compact run timestamp (e.g. "Jun 10, 14:23"). */
function formatRunTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** Read-only, selectable text with a copy button (errors / params / ids). */
function CopyableBlock({
  text,
  tone = "error",
}: {
  text: string;
  tone?: "error" | "muted";
}) {
  const { t } = useTranslation();
  const toneCls =
    tone === "error"
      ? "text-destructive bg-destructive/5 border-destructive/30"
      : "text-muted-foreground bg-muted/30";
  return (
    <div className="relative">
      <Textarea
        readOnly
        value={text}
        className={`font-mono text-[11px] leading-relaxed min-h-[7rem] max-h-80 resize-y pr-9 ${toneCls}`}
      />
      <CopyToClipboard
        text={text}
        size="icon"
        variant="ghost"
        className={`absolute top-1 right-1 size-7 ${
          tone === "error" ? "text-destructive hover:text-destructive" : ""
        }`}
        successMessage={t("common.buttons.copied")}
      />
    </div>
  );
}

/** Recursively find a block by id within a scenario's block tree (for step params). */
function findBlockById(
  blocks: ScenarioBlock[] | undefined,
  id: string,
): ScenarioBlock | null {
  if (!blocks || !id) return null;
  for (const b of blocks) {
    if (b.id === id) return b;
    const inChildren = findBlockById(b.children, id);
    if (inChildren) return inChildren;
    const inElse = findBlockById(b.branch_else, id);
    if (inElse) return inElse;
  }
  return null;
}

/** Read-only nested view of a scenario's blocks in order (for the overview dialog). */
function ScenarioFlow({
  blocks,
  depth = 0,
}: {
  blocks: ScenarioBlock[];
  depth?: number;
}) {
  const { t } = useTranslation();
  if (blocks.length === 0) {
    return (
      <span className="text-xs text-muted-foreground pl-7">
        {t("scenarios.empty")}
      </span>
    );
  }
  return (
    <div className="flex flex-col gap-1">
      {blocks.map((b, i) => (
        <div key={b.id || `${depth}-${i}`} className="flex flex-col gap-1">
          <div
            className={`flex items-center gap-2 ${b.disabled ? "opacity-50" : ""}`}
          >
            <span className="text-[10px] font-mono text-muted-foreground/60 w-5 text-right shrink-0">
              {i + 1}
            </span>
            <Badge variant="secondary" className="text-[10px] shrink-0">
              {prettify(b.type)}
            </Badge>
            <span className="text-xs text-muted-foreground truncate">
              {summarize(b)}
            </span>
          </div>
          {hasChildren(b.type) && (
            <div className="ml-6 pl-3 border-l-2 border-border flex flex-col gap-1">
              <ScenarioFlow blocks={b.children ?? []} depth={depth + 1} />
              {b.type === "condition" && (
                <>
                  <span className="text-[11px] font-medium text-muted-foreground pl-1">
                    {t("scenarios.builder.elseBranch")}
                  </span>
                  <ScenarioFlow
                    blocks={b.branch_else ?? []}
                    depth={depth + 1}
                  />
                </>
              )}
            </div>
          )}
        </div>
      ))}
    </div>
  );
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
  const [showScenarioAdv, setShowScenarioAdv] = useState(false);
  const [runProfileId, setRunProfileId] = useState<string>("");
  const [isRunning, setIsRunning] = useState(false);
  // The block editor now lives in a dialog opened from the scenario table.
  const [isEditorOpen, setIsEditorOpen] = useState(false);
  // Read-only flow overview + how-to-use guide dialogs.
  const [overviewScenario, setOverviewScenario] = useState<Scenario | null>(
    null,
  );
  const [showGuide, setShowGuide] = useState(false);
  // Confirm before flipping a schedule's enabled state from the table toggle.
  const [pendingToggle, setPendingToggle] = useState<ScenarioSchedule | null>(
    null,
  );
  const [isToggling, setIsToggling] = useState(false);
  // Which tab is active — drives the contextual header button (new scenario / new schedule).
  const [activeTab, setActiveTab] = useState<
    "editor" | "runs" | "schedules" | "ai"
  >("editor");
  // Multi-select + bulk delete for the scenario table (mirrors the Network screen).
  const [scenarioRowSelection, setScenarioRowSelection] =
    useState<RowSelectionState>({});
  const [showBulkDeleteScenarios, setShowBulkDeleteScenarios] = useState(false);
  const [isBulkDeletingScenarios, setIsBulkDeletingScenarios] = useState(false);
  // Bulk test-run: run the selected scenarios sequentially on one chosen profile.
  const [showBulkTest, setShowBulkTest] = useState(false);
  const [bulkTestProfileId, setBulkTestProfileId] = useState<string>("");
  const [isBulkTesting, setIsBulkTesting] = useState(false);

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
  // Client-side history filters (status / profile / scenario).
  const [runStatusFilter, setRunStatusFilter] = useState<
    "all" | "success" | "failed" | "stopped"
  >("all");
  const [runProfileFilter, setRunProfileFilter] = useState<string>("all");
  const [runScenarioFilter, setRunScenarioFilter] = useState<string>("all");
  // Expanded step rows (by index) in the run detail.
  const [expandedSteps, setExpandedSteps] = useState<Set<number>>(new Set());
  const [isRefreshingRuns, setIsRefreshingRuns] = useState(false);

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
  // Schedule list now uses the same table + edit-dialog pattern as the editor.
  const [isScheduleEditorOpen, setIsScheduleEditorOpen] = useState(false);
  const [scheduleRowSelection, setScheduleRowSelection] =
    useState<RowSelectionState>({});
  const [showBulkDeleteSchedules, setShowBulkDeleteSchedules] = useState(false);
  const [isBulkDeletingSchedules, setIsBulkDeletingSchedules] = useState(false);
  // Assignments per schedule (loaded with the list) → maps a schedule to its
  // profiles so we can show, per profile, which scenario is running.
  const [assignmentsBySchedule, setAssignmentsBySchedule] = useState<
    Record<string, ScenarioProfileAssignment>
  >({});
  // Profile groups (for the group-based assignment selector).
  const [groups, setGroups] = useState<GroupWithCount[]>([]);
  // Schedule rows expand to a per-profile running-status detail.
  const [expandedSchedules, setExpandedSchedules] = useState<Set<string>>(
    new Set(),
  );

  // ----- AI tab -----
  const [aiProvider, setAiProvider] = useState<ScenarioAiProvider>("anthropic");
  const [aiModel, setAiModel] = useState<string>("claude-haiku-4-5");
  const [aiApiKey, setAiApiKey] = useState<string>("");
  const [aiBaseUrl, setAiBaseUrl] = useState<string>("");
  const [aiMaxTokens, setAiMaxTokens] = useState<string>("1024");
  const [aiTemperature, setAiTemperature] = useState<string>("0.3");
  const [aiHasKey, setAiHasKey] = useState(false);
  const [aiTesting, setAiTesting] = useState(false);

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

  // Profiles a schedule targets: explicit profile_ids + members of its group_ids.
  const scheduleProfiles = useCallback(
    (scheduleId: string): string[] => {
      const asg = assignmentsBySchedule[scheduleId];
      if (!asg) return [];
      const ids = [...(asg.profile_ids ?? [])];
      if (asg.group_ids?.length) {
        for (const p of profiles) {
          if (p.group_id && asg.group_ids.includes(p.group_id)) ids.push(p.id);
        }
      }
      return [...new Set(ids)];
    },
    [assignmentsBySchedule, profiles],
  );

  // The active run currently executing on a given profile, if any.
  const runForProfile = useCallback(
    (profileId: string) => activeRuns.find((r) => r.profile_id === profileId),
    [activeRuns],
  );

  const toggleExpanded = useCallback((id: string) => {
    setExpandedSchedules((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const loadScenarios = useCallback(async () => {
    try {
      setScenarios(await invoke<Scenario[]>("scenario_list"));
    } catch (err) {
      showErrorToast(t("scenarios.errors.load", { error: String(err) }));
    }
  }, [t]);

  const loadRuns = useCallback(async (): Promise<boolean> => {
    try {
      const [list, active] = await Promise.all([
        invoke<ScenarioRunSummary[]>("scenario_list_runs", { limit: 100 }),
        invoke<ScenarioRunInfo[]>("scenario_active_runs"),
      ]);
      setRuns(list);
      setActiveRuns(active);
      return true;
    } catch (err) {
      showErrorToast(t("scenarios.errors.load", { error: String(err) }));
      return false;
    }
  }, [t]);

  // Refresh with visible feedback (spinner + toast) so it's clear it ran.
  const handleRefreshRuns = useCallback(async () => {
    setIsRefreshingRuns(true);
    try {
      if (await loadRuns()) showSuccessToast(t("scenarios.refreshed"));
    } finally {
      setIsRefreshingRuns(false);
    }
  }, [loadRuns, t]);

  const loadSchedules = useCallback(async () => {
    try {
      const list = await invoke<ScenarioSchedule[]>("scenario_list_schedules");
      setSchedules(list);
      // Load each schedule's assignment so the table can show per-profile status.
      const entries = await Promise.all(
        list.map((s) =>
          invoke<ScenarioProfileAssignment | null>("scenario_get_assignment", {
            scheduleId: s.id,
          })
            .then((a) => [s.id, a] as const)
            .catch(() => [s.id, null] as const),
        ),
      );
      const map: Record<string, ScenarioProfileAssignment> = {};
      for (const [id, a] of entries) if (a) map[id] = a;
      setAssignmentsBySchedule(map);
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
        setAiModel(cfg.model || PROVIDER_META[cfg.provider].models[0]);
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
    void invoke<GroupWithCount[]>("get_groups_with_profile_counts")
      .then(setGroups)
      .catch(() => {});
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
          setEditorMode("visual");
          setIsEditorOpen(true);
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
    setEditorMode("visual");
    setIsEditorOpen(true);
  }, []);

  // Patch scenario-level caps, filling defaults so the backend always gets a
  // complete RunCaps object even when the loaded scenario omitted some fields.
  const patchCaps = useCallback((patch: Partial<ScenarioRunCaps>) => {
    setEditorScenario((s) => ({
      ...s,
      caps: { ...DEFAULT_CAPS, ...s.caps, ...patch },
    }));
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

  // Delete a specific scenario by id (from a table row action or the editor
  // dialog footer). Confirmation runs against this id, so a half-edited buffer
  // can't retarget the delete.
  const requestDeleteScenario = useCallback(
    (id: string) => {
      setPendingDelete({
        kind: "scenario",
        id,
        name: scenarioName(id),
      });
    },
    [scenarioName],
  );

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

  // ----- Scenario table (mirrors the Network screen) -----
  const scenarioColumns = useMemo<ColumnDef<Scenario>[]>(
    () => [
      {
        id: "select",
        size: 36,
        enableSorting: false,
        header: ({ table }) => (
          <Checkbox
            checked={
              table.getIsAllRowsSelected()
                ? true
                : table.getIsSomeRowsSelected()
                  ? "indeterminate"
                  : false
            }
            onCheckedChange={(v) => table.toggleAllRowsSelected(v === true)}
            aria-label={t("common.aria.selectAll")}
          />
        ),
        cell: ({ row }) => (
          // Stop propagation so ticking the box doesn't open the editor dialog.
          // biome-ignore lint/a11y/noStaticElementInteractions: wrapper only guards row click
          // biome-ignore lint/a11y/useKeyWithClickEvents: checkbox itself remains keyboard accessible
          <div onClick={(e) => e.stopPropagation()}>
            <Checkbox
              checked={row.getIsSelected()}
              onCheckedChange={(v) => row.toggleSelected(v === true)}
              aria-label={t("common.aria.selectRow")}
            />
          </div>
        ),
      },
      {
        accessorKey: "name",
        header: () => t("common.labels.name"),
        cell: ({ row }) => (
          <span className="font-medium truncate">{row.original.name}</span>
        ),
      },
      {
        id: "blocks",
        enableSorting: false,
        header: () => t("scenarios.table.blocks"),
        cell: ({ row }) => (
          <Badge variant="secondary">
            {t("scenarios.form.blocks", {
              count: row.original.blocks?.length ?? 0,
            })}
          </Badge>
        ),
      },
      {
        id: "onError",
        enableSorting: false,
        header: () => t("scenarios.table.onError"),
        cell: ({ row }) => (
          <span className="text-xs text-muted-foreground whitespace-nowrap">
            {t(
              `scenarios.builder.onErrorOpts.${row.original.on_error ?? "stop"}`,
            )}
          </span>
        ),
      },
      ...(
        [
          ["maxSteps", "max_steps"],
          ["maxLoops", "max_loop_iterations"],
          ["maxSecs", "max_total_secs"],
          ["maxTokens", "max_ai_tokens"],
        ] as const
      ).map(([col, key]) => ({
        id: col,
        enableSorting: false,
        header: () => t(`scenarios.table.${col}`),
        cell: ({ row }: { row: Row<Scenario> }) => (
          <span className="text-xs text-muted-foreground tabular-nums whitespace-nowrap">
            {(
              row.original.caps?.[key] ??
              DEFAULT_CAPS[key] ??
              0
            ).toLocaleString()}
          </span>
        ),
      })),
      {
        id: "actions",
        enableSorting: false,
        header: () => t("common.labels.actions"),
        cell: ({ row }) => (
          <div className="flex gap-1">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-7"
                  onClick={(e) => {
                    e.stopPropagation();
                    setOverviewScenario(row.original);
                  }}
                >
                  <LuEye className="size-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("scenarios.overview")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-7"
                  onClick={(e) => {
                    e.stopPropagation();
                    void selectScenario(row.original.id);
                  }}
                >
                  <LuPencil className="size-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("common.buttons.edit")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-7 text-destructive hover:text-destructive"
                  onClick={(e) => {
                    e.stopPropagation();
                    requestDeleteScenario(row.original.id);
                  }}
                >
                  <LuTrash2 className="size-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("common.buttons.delete")}</TooltipContent>
            </Tooltip>
          </div>
        ),
      },
    ],
    [t, selectScenario, requestDeleteScenario],
  );

  const scenariosTable = useReactTable({
    data: scenarios,
    columns: scenarioColumns,
    state: { rowSelection: scenarioRowSelection },
    onRowSelectionChange: setScenarioRowSelection,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.id,
  });

  const selectedScenarios = scenariosTable
    .getFilteredSelectedRowModel()
    .rows.map((r) => r.original);

  const filteredRuns = runs.filter(
    (r) =>
      (runStatusFilter === "all" || r.status === runStatusFilter) &&
      (runProfileFilter === "all" || r.profile_id === runProfileFilter) &&
      (runScenarioFilter === "all" || r.scenario_id === runScenarioFilter),
  );
  // Distinct profiles / scenarios present in the run history (for the filters).
  const runProfileIds = [...new Set(runs.map((r) => r.profile_id))];
  const runScenarioIds = [...new Set(runs.map((r) => r.scenario_id))];
  // Blocks of the open run's scenario — to surface a step's configured params.
  const runScenarioBlocks = runDetail
    ? scenarios.find((s) => s.id === runDetail.scenario_id)?.blocks
    : undefined;

  // Bulk delete — backend has only single scenario_delete, so loop the ids.
  const handleBulkDeleteScenarios = useCallback(async () => {
    if (selectedScenarios.length === 0) return;
    setIsBulkDeletingScenarios(true);
    try {
      const results = await Promise.allSettled(
        selectedScenarios.map((s) =>
          invoke("scenario_delete", { scenarioId: s.id }),
        ),
      );
      const failed = results.filter((r) => r.status === "rejected").length;
      if (results.length - failed > 0) showSuccessToast(t("scenarios.deleted"));
      if (failed > 0) {
        showErrorToast(t("scenarios.errors.delete", { error: String(failed) }));
      }
      setScenarioRowSelection({});
      await loadScenarios();
    } finally {
      setIsBulkDeletingScenarios(false);
      setShowBulkDeleteScenarios(false);
    }
  }, [selectedScenarios, loadScenarios, t]);

  // Test the selected scenarios in sequence on a single chosen running profile.
  // (Real multi-profile/rotation runs are configured in the Schedules tab.)
  const handleBulkTest = useCallback(async () => {
    if (!bulkTestProfileId || selectedScenarios.length === 0) return;
    setIsBulkTesting(true);
    try {
      let ok = 0;
      let fail = 0;
      for (const s of selectedScenarios) {
        try {
          await invoke("scenario_run", {
            profileId: bulkTestProfileId,
            scenario: s,
          });
          ok += 1;
        } catch {
          fail += 1;
        }
      }
      showSuccessToast(t("scenarios.bulkTest.done", { ok, fail }));
      await loadRuns();
      setShowBulkTest(false);
      setScenarioRowSelection({});
    } finally {
      setIsBulkTesting(false);
    }
  }, [bulkTestProfileId, selectedScenarios, loadRuns, t]);

  const openRunDetail = useCallback(
    async (id: string) => {
      setSelectedRunId(id);
      setExpandedSteps(new Set());
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

  const scenarioOptions = useMemo<Option[]>(
    () => scenarios.map((s) => ({ label: s.name, value: s.id })),
    [scenarios],
  );

  const selectedProfileOptions = useMemo<Option[]>(
    () =>
      (asg.profile_ids ?? []).map((id) => ({
        label: profileName(id),
        value: id,
      })),
    [asg.profile_ids, profileName],
  );

  const groupName = useCallback(
    (id: string) => groups.find((g) => g.id === id)?.name ?? id,
    [groups],
  );
  const groupOptions = useMemo<Option[]>(
    () => groups.map((g) => ({ label: `${g.name} (${g.count})`, value: g.id })),
    [groups],
  );
  const selectedGroupOptions = useMemo<Option[]>(
    () =>
      (asg.group_ids ?? []).map((id) => ({ label: groupName(id), value: id })),
    [asg.group_ids, groupName],
  );

  const newSchedule = useCallback(() => {
    const tpl = newScheduleJson();
    const id = (JSON.parse(tpl) as ScenarioSchedule).id;
    setSelectedScheduleId(null);
    setScheduleJson(tpl);
    setAssignmentJson(assignmentJsonFor(id));
    setShowScheduleJson(false);
    setIsScheduleEditorOpen(true);
  }, []);

  const selectSchedule = useCallback(async (s: ScenarioSchedule) => {
    setSelectedScheduleId(s.id);
    setScheduleJson(JSON.stringify(s, null, 2));
    setShowScheduleJson(false);
    setIsScheduleEditorOpen(true);
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
      const scenarioCount =
        schedule.scenario_ids && schedule.scenario_ids.length > 0
          ? schedule.scenario_ids.length
          : schedule.scenario_id
            ? 1
            : 0;
      if (!schedule.id || scenarioCount === 0) {
        showErrorToast(t("scenarios.errors.scheduleFields"));
        return;
      }
      const assignment = JSON.parse(
        assignmentJson,
      ) as ScenarioProfileAssignment;
      // Headless removed for now — scheduled profiles always launch visible.
      assignment.run_headless = false;
      await invoke("scenario_save_schedule", { schedule });
      if (assignment.schedule_id) {
        await invoke("scenario_save_assignment", { assignment });
      }
      showSuccessToast(t("scenarios.scheduleSaved"));
      setIsScheduleEditorOpen(false);
      await loadSchedules();
    } catch (err) {
      showErrorToast(t("scenarios.errors.json", { error: String(err) }));
    }
  }, [scheduleJson, assignmentJson, loadSchedules, t]);

  const requestDeleteSchedule = useCallback((id: string, name: string) => {
    setPendingDelete({ kind: "schedule", id, name });
  }, []);

  // Flip enabled directly from the table row (saves just the schedule).
  const toggleScheduleEnabled = useCallback(
    async (schedule: ScenarioSchedule) => {
      try {
        await invoke("scenario_save_schedule", {
          schedule: { ...schedule, enabled: !schedule.enabled },
        });
        await loadSchedules();
      } catch (err) {
        showErrorToast(t("scenarios.errors.save", { error: String(err) }));
      }
    },
    [loadSchedules, t],
  );

  // Apply the pending enable/disable after the user confirms.
  const confirmToggle = useCallback(async () => {
    if (!pendingToggle) return;
    setIsToggling(true);
    try {
      await toggleScheduleEnabled(pendingToggle);
    } finally {
      setIsToggling(false);
      setPendingToggle(null);
    }
  }, [pendingToggle, toggleScheduleEnabled]);

  const scheduleColumns = useMemo<ColumnDef<ScenarioSchedule>[]>(
    () => [
      {
        id: "select",
        size: 36,
        enableSorting: false,
        header: ({ table }) => (
          <Checkbox
            checked={
              table.getIsAllRowsSelected()
                ? true
                : table.getIsSomeRowsSelected()
                  ? "indeterminate"
                  : false
            }
            onCheckedChange={(v) => table.toggleAllRowsSelected(v === true)}
            aria-label={t("common.aria.selectAll")}
          />
        ),
        cell: ({ row }) => (
          // biome-ignore lint/a11y/noStaticElementInteractions: wrapper only guards row click
          // biome-ignore lint/a11y/useKeyWithClickEvents: checkbox stays keyboard accessible
          <div onClick={(e) => e.stopPropagation()}>
            <Checkbox
              checked={row.getIsSelected()}
              onCheckedChange={(v) => row.toggleSelected(v === true)}
              aria-label={t("common.aria.selectRow")}
            />
          </div>
        ),
      },
      {
        id: "status",
        size: 24,
        enableSorting: false,
        header: () => null,
        cell: ({ row }) => (
          <span
            className={`size-2 rounded-full block ${
              row.original.enabled ? "bg-success" : "bg-muted-foreground/40"
            }`}
          />
        ),
      },
      {
        accessorKey: "name",
        header: () => t("common.labels.name"),
        cell: ({ row }) => {
          const id = row.original.id;
          const runningCount = scheduleProfiles(id).filter((pid) =>
            runForProfile(pid),
          ).length;
          return (
            <span className="flex items-center gap-1.5">
              {expandedSchedules.has(id) ? (
                <LuChevronDown className="size-3.5 text-muted-foreground shrink-0" />
              ) : (
                <LuChevronRight className="size-3.5 text-muted-foreground shrink-0" />
              )}
              <span className="font-medium truncate">{row.original.name}</span>
              {runningCount > 0 && (
                <span className="flex items-center gap-1 text-[11px] text-success shrink-0">
                  <span className="size-1.5 rounded-full bg-success animate-pulse" />
                  {runningCount}
                </span>
              )}
            </span>
          );
        },
      },
      {
        id: "scenarios",
        enableSorting: false,
        header: () => t("scenarios.form.scenarios"),
        cell: ({ row }) => {
          const n =
            row.original.scenario_ids && row.original.scenario_ids.length > 0
              ? row.original.scenario_ids.length
              : row.original.scenario_id
                ? 1
                : 0;
          return (
            <Badge variant="secondary">
              {t("scenarios.table.scenarioCount", { count: n })}
            </Badge>
          );
        },
      },
      {
        id: "trigger",
        enableSorting: false,
        header: () => t("scenarios.form.trigger"),
        cell: ({ row }) => {
          const s = row.original;
          let label = t(`scenarios.form.triggerOpts.${s.trigger_type}`);
          if (s.trigger_type === "interval" && s.interval_minutes) {
            label = t("scenarios.everyMinutes", { count: s.interval_minutes });
          } else if (s.trigger_type === "cron" && s.cron_expr) {
            label = s.cron_expr;
          }
          return (
            <span className="text-xs text-muted-foreground whitespace-nowrap">
              {label}
            </span>
          );
        },
      },
      {
        id: "profiles",
        enableSorting: false,
        header: () => t("scenarios.form.profiles"),
        cell: ({ row }) => {
          const pids = scheduleProfiles(row.original.id);
          if (pids.length === 0) {
            return <span className="text-xs text-muted-foreground">—</span>;
          }
          // Running profiles first so they stay visible within the 2 shown.
          const ordered = [...pids].sort(
            (a, b) => (runForProfile(b) ? 1 : 0) - (runForProfile(a) ? 1 : 0),
          );
          const shown = ordered.slice(0, 2);
          const rest = ordered.slice(2);
          return (
            <div className="flex flex-wrap items-center gap-1">
              {shown.map((pid) => {
                const active = !!runForProfile(pid);
                return (
                  <Badge
                    key={pid}
                    variant="secondary"
                    className={cn(
                      "max-w-28 truncate font-normal",
                      active &&
                        "bg-blue-500/15 text-blue-600 border-blue-500/40 dark:text-blue-300",
                    )}
                  >
                    {profileName(pid)}
                  </Badge>
                );
              })}
              {rest.length > 0 && (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <Badge
                      variant="outline"
                      className="text-muted-foreground cursor-default"
                    >
                      +{rest.length}
                    </Badge>
                  </TooltipTrigger>
                  <TooltipContent className="max-w-xs">
                    {rest.map((pid) => profileName(pid)).join(", ")}
                  </TooltipContent>
                </Tooltip>
              )}
            </div>
          );
        },
      },
      {
        id: "rotation",
        enableSorting: false,
        header: () => t("scenarios.form.rotation"),
        cell: ({ row }) => {
          const asg = assignmentsBySchedule[row.original.id];
          return (
            <span className="text-xs text-muted-foreground whitespace-nowrap">
              {asg
                ? t(`scenarios.form.rotationOpts.${asg.rotation_mode}`)
                : "—"}
            </span>
          );
        },
      },
      {
        id: "parallel",
        enableSorting: false,
        header: () => t("scenarios.table.parallel"),
        cell: ({ row }) => {
          const asg = assignmentsBySchedule[row.original.id];
          return (
            <span className="text-xs text-muted-foreground tabular-nums text-center block">
              {asg ? asg.max_parallel : "—"}
            </span>
          );
        },
      },
      {
        id: "cooldown",
        enableSorting: false,
        header: () => t("scenarios.table.cooldown"),
        cell: ({ row }) => {
          const asg = assignmentsBySchedule[row.original.id];
          return (
            <span className="text-xs text-muted-foreground tabular-nums text-center block">
              {asg ? (asg.cooldown_minutes ?? 0) : "—"}
            </span>
          );
        },
      },
      {
        id: "actions",
        enableSorting: false,
        header: () => t("common.labels.actions"),
        cell: ({ row }) => (
          <div className="flex items-center gap-1.5">
            <Tooltip>
              <TooltipTrigger asChild>
                {/* biome-ignore lint/a11y/noStaticElementInteractions: guards row toggle */}
                {/* biome-ignore lint/a11y/useKeyWithClickEvents: switch stays keyboard accessible */}
                <span
                  className="flex items-center mr-1"
                  onClick={(e) => e.stopPropagation()}
                >
                  <AnimatedSwitch
                    checked={row.original.enabled}
                    onCheckedChange={() => setPendingToggle(row.original)}
                  />
                </span>
              </TooltipTrigger>
              <TooltipContent>
                {t("scenarios.scheduleToggleTooltip")}
              </TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-7"
                  onClick={(e) => {
                    e.stopPropagation();
                    void selectSchedule(row.original);
                  }}
                >
                  <LuPencil className="size-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("common.buttons.edit")}</TooltipContent>
            </Tooltip>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-7 text-destructive hover:text-destructive"
                  onClick={(e) => {
                    e.stopPropagation();
                    requestDeleteSchedule(row.original.id, row.original.name);
                  }}
                >
                  <LuTrash2 className="size-3.5" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t("common.buttons.delete")}</TooltipContent>
            </Tooltip>
          </div>
        ),
      },
    ],
    [
      t,
      selectSchedule,
      requestDeleteSchedule,
      expandedSchedules,
      scheduleProfiles,
      runForProfile,
      profileName,
      assignmentsBySchedule,
    ],
  );

  const schedulesTable = useReactTable({
    data: schedules,
    columns: scheduleColumns,
    state: { rowSelection: scheduleRowSelection },
    onRowSelectionChange: setScheduleRowSelection,
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.id,
  });

  const selectedSchedules = schedulesTable
    .getFilteredSelectedRowModel()
    .rows.map((r) => r.original);

  // Bulk delete schedules — loop the single delete command over the selection.
  const handleBulkDeleteSchedules = useCallback(async () => {
    if (selectedSchedules.length === 0) return;
    setIsBulkDeletingSchedules(true);
    try {
      const results = await Promise.allSettled(
        selectedSchedules.map((s) =>
          invoke("scenario_delete_schedule", { scheduleId: s.id }),
        ),
      );
      const failed = results.filter((r) => r.status === "rejected").length;
      if (results.length - failed > 0) {
        showSuccessToast(t("scenarios.scheduleDeleted"));
      }
      if (failed > 0) {
        showErrorToast(t("scenarios.errors.delete", { error: String(failed) }));
      }
      setScheduleRowSelection({});
      await loadSchedules();
    } finally {
      setIsBulkDeletingSchedules(false);
      setShowBulkDeleteSchedules(false);
    }
  }, [selectedSchedules, loadSchedules, t]);

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
        setIsEditorOpen(false);
        showSuccessToast(t("scenarios.deleted"));
        await loadScenarios();
      } else {
        await invoke("scenario_delete_schedule", {
          scheduleId: pendingDelete.id,
        });
        setSelectedScheduleId(null);
        setScheduleJson(newScheduleJson());
        setAssignmentJson(assignmentJsonFor(""));
        setIsScheduleEditorOpen(false);
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

  // Probe the provider with a tiny request. Empty key → backend uses the stored
  // one, so a saved key can be tested without retyping it.
  const handleTestAi = useCallback(async () => {
    setAiTesting(true);
    try {
      const reply = await invoke<string>("scenario_test_ai_provider", {
        config: {
          provider: aiProvider,
          model: aiModel,
          api_key: aiApiKey,
          base_url: aiBaseUrl || null,
          max_tokens: Number(aiMaxTokens) || 1024,
          temperature: Number(aiTemperature) || 0.3,
        },
      });
      showSuccessToast(t("scenarios.aiTestOk"), { description: reply.trim() });
    } catch (err) {
      showErrorToast(t("scenarios.aiTestFailed", { error: String(err) }));
    } finally {
      setAiTesting(false);
    }
  }, [aiProvider, aiModel, aiApiKey, aiBaseUrl, aiMaxTokens, aiTemperature, t]);

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
        {!subPage ? (
          <DialogHeader className="shrink-0">
            <DialogTitle>{t("scenarios.title")}</DialogTitle>
            <DialogDescription className="sr-only">
              {t("scenarios.title")}
            </DialogDescription>
          </DialogHeader>
        ) : (
          <DialogHeader className="sr-only">
            <DialogTitle>{t("scenarios.title")}</DialogTitle>
            <DialogDescription>{t("scenarios.title")}</DialogDescription>
          </DialogHeader>
        )}

        <div className="overflow-hidden flex-1 min-h-0 flex flex-col">
          <AnimatedTabs
            defaultValue="editor"
            onValueChange={(v) =>
              setActiveTab(v as "editor" | "runs" | "schedules" | "ai")
            }
            className="flex flex-col flex-1 min-h-0"
          >
            {/* Tabs + contextual "new" button on the same row (like the Network screen). */}
            <div className="flex items-center justify-between gap-3 shrink-0">
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
              <div className="flex items-center gap-2">
                {activeTab === "editor" && (
                  <>
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => setShowGuide(true)}
                    >
                      <LuBookOpen className="size-3.5" /> {t("scenarios.guide")}
                    </Button>
                    <Button size="sm" onClick={handleNew}>
                      <LuPlus className="size-3.5" />{" "}
                      {t("scenarios.newScenario")}
                    </Button>
                  </>
                )}
                {activeTab === "schedules" && (
                  <Button size="sm" onClick={newSchedule}>
                    <LuPlus className="size-3.5" /> {t("scenarios.newSchedule")}
                  </Button>
                )}
              </div>
            </div>

            {/* ---------- Editor: scenario table ---------- */}
            <AnimatedTabsContent
              value="editor"
              className="mt-4 flex-1 min-h-0 data-[state=active]:flex flex-col"
            >
              <div className="flex flex-col gap-4 flex-1 min-h-0">
                {scenarios.length === 0 ? (
                  <p className="text-sm text-muted-foreground px-1 py-3">
                    {t("scenarios.empty")}
                  </p>
                ) : (
                  <FadingScrollArea
                    className="flex-1 min-h-0"
                    style={
                      {
                        "--scroll-fade-top-offset": "32px",
                      } as React.CSSProperties
                    }
                  >
                    <Table className="w-full">
                      <TableHeader className="sticky top-0 z-10 bg-background">
                        {scenariosTable.getHeaderGroups().map((headerGroup) => (
                          <TableRow key={headerGroup.id}>
                            {headerGroup.headers.map((header) => (
                              <TableHead
                                key={header.id}
                                style={{
                                  width: header.column.columnDef.size
                                    ? `${header.column.getSize()}px`
                                    : undefined,
                                }}
                                className={cn(
                                  header.column.id !== "name" &&
                                    header.column.id !== "select" &&
                                    header.column.id !== "profiles" &&
                                    "whitespace-nowrap w-px",
                                )}
                              >
                                {header.isPlaceholder
                                  ? null
                                  : flexRender(
                                      header.column.columnDef.header,
                                      header.getContext(),
                                    )}
                              </TableHead>
                            ))}
                          </TableRow>
                        ))}
                      </TableHeader>
                      <TableBody>
                        {scenariosTable.getRowModel().rows.map((row) => (
                          <TableRow
                            key={row.id}
                            data-state={row.getIsSelected() && "selected"}
                          >
                            {row.getVisibleCells().map((cell) => (
                              <TableCell
                                key={cell.id}
                                style={{
                                  width: cell.column.columnDef.size
                                    ? `${cell.column.getSize()}px`
                                    : undefined,
                                }}
                              >
                                {flexRender(
                                  cell.column.columnDef.cell,
                                  cell.getContext(),
                                )}
                              </TableCell>
                            ))}
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </FadingScrollArea>
                )}
              </div>
            </AnimatedTabsContent>

            {/* ---------- Edit-scenario dialog (rendered as a sibling so tab
                switches don't unmount it) ---------- */}
            <Dialog open={isEditorOpen} onOpenChange={setIsEditorOpen}>
              {/* Fixed height so switching Visual↔JSON doesn't resize the dialog. */}
              <DialogContent className="max-w-3xl h-[85vh] flex flex-col">
                <DialogHeader>
                  <DialogTitle>
                    {editorMode === "visual" && editorScenario.name
                      ? editorScenario.name
                      : t("scenarios.editScenarioTitle")}
                  </DialogTitle>
                  <DialogDescription className="sr-only">
                    {t("scenarios.editScenarioTitle")}
                  </DialogDescription>
                </DialogHeader>
                <section className="flex-1 min-w-0 flex flex-col gap-3 min-h-0">
                  <div className="flex items-center justify-end gap-2">
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

                      {/* Scenario-level error policy + safety caps */}
                      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
                        <span className="flex items-center gap-2">
                          <FieldLabel>
                            {t("scenarios.builder.onError")}
                          </FieldLabel>
                          <Select
                            value={editorScenario.on_error ?? "stop"}
                            onValueChange={(v) =>
                              setEditorScenario({
                                ...editorScenario,
                                on_error: v as ScenarioOnError,
                              })
                            }
                          >
                            <SelectTrigger className="h-7 w-28 text-xs">
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              {ON_ERROR_MODES.map((m) => (
                                <SelectItem key={m} value={m}>
                                  {t(`scenarios.builder.onErrorOpts.${m}`)}
                                </SelectItem>
                              ))}
                            </SelectContent>
                          </Select>
                        </span>
                        <button
                          type="button"
                          onClick={() => setShowScenarioAdv((v) => !v)}
                          className="flex items-center gap-1 text-[11px] text-muted-foreground hover:text-foreground transition-colors"
                        >
                          {showScenarioAdv ? (
                            <LuChevronDown className="size-3.5" />
                          ) : (
                            <LuChevronRight className="size-3.5" />
                          )}
                          {t("scenarios.builder.caps")}
                        </button>
                      </div>
                      {showScenarioAdv && (
                        <div className="grid grid-cols-2 gap-3 rounded-md border bg-muted/20 p-3">
                          {(
                            [
                              "max_steps",
                              "max_loop_iterations",
                              "max_total_secs",
                              "max_ai_tokens",
                            ] as const
                          ).map((key) => (
                            <div key={key} className="flex flex-col gap-1">
                              <FieldLabel>
                                {t(`scenarios.builder.capsField.${key}`)}
                              </FieldLabel>
                              <Input
                                type="number"
                                min={0}
                                value={
                                  (editorScenario.caps ?? DEFAULT_CAPS)[key] ??
                                  0
                                }
                                onChange={(e) =>
                                  patchCaps({
                                    [key]: Number(e.target.value) || 0,
                                  })
                                }
                                className="h-7 text-xs"
                              />
                            </div>
                          ))}
                        </div>
                      )}

                      <BlockEditor
                        blocks={editorScenario.blocks}
                        onChange={(blocks) =>
                          setEditorScenario({ ...editorScenario, blocks })
                        }
                      />
                    </div>
                  )}

                  {runnableProfiles.length === 0 && (
                    <p className="text-xs text-muted-foreground -mt-1">
                      {t("scenarios.noRunningProfiles")}
                    </p>
                  )}
                </section>
                {/* Footer: save / delete on the left, single-profile Test on the right.
                    Real multi-profile runs are configured in the Schedules tab. */}
                <DialogFooter className="flex-row flex-wrap items-center gap-2 border-t pt-3 sm:justify-start">
                  <Button size="sm" onClick={() => void handleSaveScenario()}>
                    <LuSave className="size-3.5" /> {t("scenarios.save")}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    className="text-destructive hover:text-destructive"
                    disabled={!selectedId}
                    onClick={() => requestDeleteScenario(editorScenario.id)}
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
                      <LuPlay className="size-3.5" /> {t("scenarios.test")}
                    </Button>
                  </div>
                </DialogFooter>
              </DialogContent>
            </Dialog>

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
                      disabled={isRefreshingRuns}
                      onClick={() => void handleRefreshRuns()}
                    >
                      <LuRefreshCw
                        className={`size-3.5 ${isRefreshingRuns ? "animate-spin" : ""}`}
                      />{" "}
                      {t("scenarios.refresh")}
                    </Button>
                  </div>
                  {/* Status filter */}
                  <div className="flex items-center gap-1 px-1">
                    {(["all", "success", "failed", "stopped"] as const).map(
                      (s) => (
                        <button
                          key={s}
                          type="button"
                          onClick={() => setRunStatusFilter(s)}
                          className={`px-2 py-0.5 rounded text-[11px] font-medium transition-colors ${
                            runStatusFilter === s
                              ? "bg-primary text-primary-foreground"
                              : "text-muted-foreground hover:bg-accent"
                          }`}
                        >
                          {s === "all"
                            ? t("scenarios.filterAll")
                            : t(`scenarios.statusLabels.${s}`)}
                        </button>
                      ),
                    )}
                  </div>
                  {/* Profile + scenario filters */}
                  <div className="flex items-center gap-1 px-1">
                    <Select
                      value={runProfileFilter}
                      onValueChange={setRunProfileFilter}
                    >
                      <SelectTrigger className="h-7 text-xs flex-1 min-w-0 [&>span]:truncate">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="all">
                          {t("scenarios.form.profiles")}
                        </SelectItem>
                        {runProfileIds.map((id) => (
                          <SelectItem key={id} value={id}>
                            {profileName(id)}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <Select
                      value={runScenarioFilter}
                      onValueChange={setRunScenarioFilter}
                    >
                      <SelectTrigger className="h-7 text-xs flex-1 min-w-0 [&>span]:truncate">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="all">
                          {t("scenarios.form.scenarios")}
                        </SelectItem>
                        {runScenarioIds.map((id) => (
                          <SelectItem key={id} value={id}>
                            {scenarioName(id)}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                  <div className="flex flex-col gap-1 overflow-y-auto min-h-0 flex-1 pr-1">
                    {filteredRuns.map((r) => {
                      const active = selectedRunId === r.id;
                      return (
                        <button
                          key={r.id}
                          type="button"
                          onClick={() => void openRunDetail(r.id)}
                          className={`text-left px-3 py-2.5 rounded-lg flex flex-col gap-1.5 border transition-colors ${
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
                          <span className="flex items-center gap-2 text-[11px] text-muted-foreground">
                            <span className="truncate flex-1">
                              {scenarioName(r.scenario_id)}
                            </span>
                            <span className="shrink-0">
                              {formatRunTime(r.started_at)}
                            </span>
                          </span>
                          <span className="flex items-center gap-2.5 text-[11px] font-mono text-muted-foreground">
                            <span className="text-success">{r.steps_ok}✓</span>
                            {r.steps_failed > 0 && (
                              <span className="text-destructive">
                                {r.steps_failed}✗
                              </span>
                            )}
                            <span className="ml-auto flex items-center gap-1">
                              <LuClock className="size-3" />
                              {formatDuration(r.duration_ms)}
                            </span>
                          </span>
                        </button>
                      );
                    })}
                    {filteredRuns.length === 0 && (
                      <p className="text-xs text-muted-foreground px-2 py-3 text-center">
                        {t("scenarios.noRuns")}
                      </p>
                    )}
                  </div>
                </aside>

                <div className="flex-1 min-w-0 rounded-lg border bg-card overflow-y-auto">
                  {runDetail ? (
                    <div className="p-4">
                      <div className="flex flex-col gap-2 pb-3 mb-3 border-b">
                        <div className="flex items-center gap-2">
                          <span
                            className={`px-1.5 py-0.5 rounded text-[10px] font-medium shrink-0 ${statusTone(runDetail.status)}`}
                          >
                            {t(`scenarios.statusLabels.${runDetail.status}`)}
                          </span>
                          <span className="text-sm font-medium truncate">
                            {scenarioName(runDetail.scenario_id)}
                          </span>
                          <span className="ml-auto flex items-center gap-1 text-xs text-muted-foreground font-mono shrink-0">
                            <LuClock className="size-3" />
                            {formatDuration(runDetail.duration_ms)}
                          </span>
                        </div>
                        <div className="flex items-center flex-wrap gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
                          <span className="flex items-center gap-1 min-w-0">
                            <LuGlobe className="size-3 shrink-0" />
                            <span className="truncate">
                              {profileName(runDetail.profile_id)}
                            </span>
                          </span>
                          <Badge
                            variant="outline"
                            className="text-[10px] font-normal"
                          >
                            {t(
                              `scenarios.triggeredByOpts.${runDetail.triggered_by}`,
                            )}
                          </Badge>
                          <span>{formatRunTime(runDetail.started_at)}</span>
                          <span className="ml-auto flex items-center gap-2 font-mono">
                            <span>{runDetail.steps.length}</span>
                            <span className="text-success">
                              {
                                runDetail.steps.filter((s) => s.status === "ok")
                                  .length
                              }
                              ✓
                            </span>
                            {runDetail.steps.some(
                              (s) => s.status === "failed",
                            ) && (
                              <span className="text-destructive">
                                {
                                  runDetail.steps.filter(
                                    (s) => s.status === "failed",
                                  ).length
                                }
                                ✗
                              </span>
                            )}
                          </span>
                        </div>
                        {runDetail.error && (
                          <CopyableBlock text={runDetail.error} />
                        )}
                      </div>
                      <div className="flex flex-col">
                        {runDetail.steps.map((s, i) => {
                          const expanded = expandedSteps.has(i);
                          // Prefer id match; fall back to position (flat scenarios
                          // whose blocks have no id), guarded by matching type.
                          let block = findBlockById(
                            runScenarioBlocks,
                            s.block_id,
                          );
                          if (
                            !block &&
                            runScenarioBlocks?.[i]?.type === s.block_type
                          ) {
                            block = runScenarioBlocks[i];
                          }
                          const hasParams =
                            !!block?.params &&
                            typeof block.params === "object" &&
                            Object.keys(block.params as object).length > 0;
                          return (
                            <div
                              key={`${s.block_id}-${i}`}
                              className="border-b border-border/50 last:border-0"
                            >
                              <button
                                type="button"
                                onClick={() =>
                                  setExpandedSteps((prev) => {
                                    const n = new Set(prev);
                                    if (n.has(i)) n.delete(i);
                                    else n.add(i);
                                    return n;
                                  })
                                }
                                className="w-full text-xs flex items-center gap-2 py-1.5 px-1 text-left rounded hover:bg-accent/40 transition-colors"
                              >
                                <span className="text-[10px] text-muted-foreground/60 font-mono w-5 shrink-0 text-right">
                                  {i + 1}
                                </span>
                                {s.status === "ok" ? (
                                  <LuCircleCheck className="size-3.5 text-success shrink-0" />
                                ) : s.status === "failed" ? (
                                  <LuBan className="size-3.5 text-destructive shrink-0" />
                                ) : s.status === "retried" ? (
                                  <LuRefreshCw className="size-3.5 text-warning shrink-0" />
                                ) : s.status === "dry_run" ? (
                                  <LuEye className="size-3.5 text-blue-500 shrink-0" />
                                ) : s.status === "skipped" ? (
                                  <span className="size-3.5 grid place-items-center text-muted-foreground shrink-0">
                                    –
                                  </span>
                                ) : (
                                  <span className="size-1.5 rounded-full bg-muted-foreground/40 shrink-0 mx-1" />
                                )}
                                <span className="truncate">
                                  {prettify(s.block_type)}
                                </span>
                                <span className="text-muted-foreground font-mono ml-auto shrink-0">
                                  {formatDuration(s.duration_ms)}
                                </span>
                                {expanded ? (
                                  <LuChevronDown className="size-3.5 text-muted-foreground shrink-0" />
                                ) : (
                                  <LuChevronRight className="size-3.5 text-muted-foreground shrink-0" />
                                )}
                              </button>
                              {expanded && (
                                <div className="pl-9 pr-1 pb-2.5 flex flex-col gap-2">
                                  <div className="flex flex-wrap gap-x-4 gap-y-1 text-[11px] text-muted-foreground font-mono">
                                    <span>{s.status}</span>
                                    <span>{formatDuration(s.duration_ms)}</span>
                                    {s.block_id && (
                                      <span className="truncate">
                                        ID: {s.block_id}
                                      </span>
                                    )}
                                  </div>
                                  {hasParams && (
                                    <div className="flex flex-col gap-1">
                                      <span className="text-[11px] font-medium text-muted-foreground">
                                        {t("scenarios.stepParams")}
                                      </span>
                                      <CopyableBlock
                                        tone="muted"
                                        text={JSON.stringify(
                                          block?.params,
                                          null,
                                          2,
                                        )}
                                      />
                                    </div>
                                  )}
                                  {s.error && <CopyableBlock text={s.error} />}
                                </div>
                              )}
                            </div>
                          );
                        })}
                      </div>
                      {runDetail.warnings.length > 0 && (
                        <p className="text-[11px] text-warning mt-3 border-t pt-2">
                          {runDetail.warnings.join("; ")}
                        </p>
                      )}
                    </div>
                  ) : (
                    <div className="h-full grid place-items-center p-8">
                      <div className="flex flex-col items-center gap-2 text-center">
                        <LuClock className="size-7 text-muted-foreground/40" />
                        <p className="text-sm text-muted-foreground">
                          {t("scenarios.form.selectRun")}
                        </p>
                      </div>
                    </div>
                  )}
                </div>
              </div>
            </AnimatedTabsContent>

            {/* ---------- Schedules: table ---------- */}
            <AnimatedTabsContent
              value="schedules"
              className="mt-4 flex-1 min-h-0 data-[state=active]:flex flex-col"
            >
              <div className="flex flex-col gap-4 flex-1 min-h-0">
                {schedules.length === 0 ? (
                  <p className="text-sm text-muted-foreground px-1 py-3">
                    {t("scenarios.empty")}
                  </p>
                ) : (
                  <FadingScrollArea
                    className="flex-1 min-h-0"
                    style={
                      {
                        "--scroll-fade-top-offset": "32px",
                      } as React.CSSProperties
                    }
                  >
                    <Table className="w-full">
                      <TableHeader className="sticky top-0 z-10 bg-background">
                        {schedulesTable.getHeaderGroups().map((headerGroup) => (
                          <TableRow key={headerGroup.id}>
                            {headerGroup.headers.map((header) => (
                              <TableHead
                                key={header.id}
                                style={{
                                  width: header.column.columnDef.size
                                    ? `${header.column.getSize()}px`
                                    : undefined,
                                }}
                                className={cn(
                                  header.column.id !== "name" &&
                                    header.column.id !== "select" &&
                                    header.column.id !== "profiles" &&
                                    "whitespace-nowrap w-px",
                                )}
                              >
                                {header.isPlaceholder
                                  ? null
                                  : flexRender(
                                      header.column.columnDef.header,
                                      header.getContext(),
                                    )}
                              </TableHead>
                            ))}
                          </TableRow>
                        ))}
                      </TableHeader>
                      <TableBody>
                        {schedulesTable.getRowModel().rows.map((row) => (
                          <Fragment key={row.id}>
                            <TableRow
                              data-state={row.getIsSelected() && "selected"}
                              className="cursor-pointer"
                              onClick={() => toggleExpanded(row.original.id)}
                            >
                              {row.getVisibleCells().map((cell) => (
                                <TableCell
                                  key={cell.id}
                                  style={{
                                    width: cell.column.columnDef.size
                                      ? `${cell.column.getSize()}px`
                                      : undefined,
                                  }}
                                >
                                  {flexRender(
                                    cell.column.columnDef.cell,
                                    cell.getContext(),
                                  )}
                                </TableCell>
                              ))}
                            </TableRow>
                            {expandedSchedules.has(row.original.id) && (
                              <TableRow className="hover:bg-transparent">
                                <TableCell
                                  colSpan={scheduleColumns.length}
                                  className="bg-muted/30 p-3"
                                >
                                  {scheduleProfiles(row.original.id).length ===
                                  0 ? (
                                    <span className="text-xs text-muted-foreground pl-8">
                                      {t("scenarios.empty")}
                                    </span>
                                  ) : (
                                    <div className="grid grid-cols-2 gap-2 pl-8">
                                      {scheduleProfiles(row.original.id).map(
                                        (pid) => {
                                          const run = runForProfile(pid);
                                          return (
                                            <div
                                              key={pid}
                                              className="flex items-center gap-2.5 rounded-lg border bg-card px-3 py-2"
                                            >
                                              <span
                                                className={`grid place-items-center size-7 rounded-md shrink-0 ${
                                                  run
                                                    ? "bg-success/15 text-success"
                                                    : "bg-muted text-muted-foreground"
                                                }`}
                                              >
                                                <LuGlobe className="size-3.5" />
                                              </span>
                                              <span className="flex flex-col min-w-0 flex-1 gap-0.5">
                                                <span className="text-xs font-medium truncate">
                                                  {profileName(pid)}
                                                </span>
                                                {run ? (
                                                  <span className="flex items-center gap-1 text-[11px] text-success">
                                                    <span className="size-1.5 rounded-full bg-success animate-pulse shrink-0" />
                                                    <span className="truncate">
                                                      {t(
                                                        "scenarios.runningScenario",
                                                        {
                                                          name: scenarioName(
                                                            run.scenario_id,
                                                          ),
                                                        },
                                                      )}
                                                    </span>
                                                  </span>
                                                ) : (
                                                  <span className="text-[11px] text-muted-foreground">
                                                    {t("scenarios.idle")}
                                                  </span>
                                                )}
                                              </span>
                                            </div>
                                          );
                                        },
                                      )}
                                    </div>
                                  )}
                                </TableCell>
                              </TableRow>
                            )}
                          </Fragment>
                        ))}
                      </TableBody>
                    </Table>
                  </FadingScrollArea>
                )}
              </div>
            </AnimatedTabsContent>

            {/* ---------- Edit-schedule dialog ---------- */}
            <Dialog
              open={isScheduleEditorOpen}
              onOpenChange={setIsScheduleEditorOpen}
            >
              <DialogContent className="max-w-2xl max-h-[88vh] flex flex-col">
                <DialogHeader>
                  <DialogTitle>
                    {sched.name || t("scenarios.form.scheduleName")}
                  </DialogTitle>
                  <DialogDescription className="sr-only">
                    {t("scenarios.tabSchedules")}
                  </DialogDescription>
                </DialogHeader>
                <section className="flex-1 min-w-0 flex flex-col gap-3 min-h-0">
                  <div className="flex-1 min-h-0 overflow-y-auto pr-1 flex flex-col gap-4">
                    {/* Schedule fields */}
                    <div className="rounded-lg border bg-card p-4 flex flex-col gap-3">
                      {/* Enable/disable is controlled from the schedule table toggle. */}
                      <Input
                        value={sched.name ?? ""}
                        onChange={(e) => patchSched({ name: e.target.value })}
                        placeholder={t("scenarios.form.scheduleName")}
                        className="h-8 text-sm font-medium max-w-sm"
                      />

                      <div className="grid grid-cols-2 gap-3">
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.scenarios")}
                          </FieldLabel>
                          <MultipleSelector
                            value={(sched.scenario_ids &&
                            sched.scenario_ids.length > 0
                              ? sched.scenario_ids
                              : sched.scenario_id
                                ? [sched.scenario_id]
                                : []
                            ).map((id) => ({
                              label: scenarioName(id),
                              value: id,
                            }))}
                            defaultOptions={scenarioOptions}
                            options={scenarioOptions}
                            placeholder={
                              scenarios.length === 0
                                ? t("scenarios.form.noScenarios")
                                : t("scenarios.form.pickScenarios")
                            }
                            hidePlaceholderWhenSelected
                            onChange={(opts) =>
                              patchSched({
                                scenario_ids: opts.map((o) => o.value),
                                scenario_id: "",
                              })
                            }
                            emptyIndicator={
                              <p className="text-center text-xs text-muted-foreground py-2">
                                {t("scenarios.empty")}
                              </p>
                            }
                          />
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
                        <>
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
                          <div className="flex flex-col gap-1">
                            <FieldLabel>
                              {t("scenarios.form.timezone")}
                            </FieldLabel>
                            <Input
                              value={sched.timezone ?? ""}
                              onChange={(e) =>
                                patchSched({
                                  timezone: e.target.value || undefined,
                                })
                              }
                              placeholder="Asia/Ho_Chi_Minh"
                              className="h-8 font-mono text-xs"
                            />
                            <span className="text-[11px] text-muted-foreground">
                              {t("scenarios.form.timezoneHint")}
                            </span>
                          </div>
                        </>
                      )}

                      <div className="grid grid-cols-3 gap-3">
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.windowStart")}
                          </FieldLabel>
                          <Input
                            type="time"
                            value={sched.time_window_start ?? ""}
                            onChange={(e) =>
                              patchSched({
                                time_window_start: e.target.value || undefined,
                              })
                            }
                            className="h-8"
                          />
                        </div>
                        <div className="flex flex-col gap-1">
                          <FieldLabel>
                            {t("scenarios.form.windowEnd")}
                          </FieldLabel>
                          <Input
                            type="time"
                            value={sched.time_window_end ?? ""}
                            onChange={(e) =>
                              patchSched({
                                time_window_end: e.target.value || undefined,
                              })
                            }
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
                      <div className="flex flex-col gap-1">
                        <FieldLabel>{t("scenarios.form.groups")}</FieldLabel>
                        <MultipleSelector
                          value={selectedGroupOptions}
                          defaultOptions={groupOptions}
                          options={groupOptions}
                          placeholder={t("scenarios.form.pickGroups")}
                          hidePlaceholderWhenSelected
                          onChange={(opts) =>
                            patchAsg({ group_ids: opts.map((o) => o.value) })
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

                  <p className="text-xs text-muted-foreground">
                    {t("scenarios.scheduleHint")}
                  </p>
                </section>
                {/* Footer — one Save persists both schedule + assignment */}
                <DialogFooter className="flex-row flex-wrap items-center gap-2 border-t pt-3 sm:justify-start">
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
                    onClick={() =>
                      selectedScheduleId &&
                      requestDeleteSchedule(
                        selectedScheduleId,
                        sched.name ?? selectedScheduleId,
                      )
                    }
                  >
                    <LuTrash2 className="size-3.5" /> {t("scenarios.delete")}
                  </Button>
                </DialogFooter>
              </DialogContent>
            </Dialog>

            {/* ---------- AI ---------- */}
            <AnimatedTabsContent
              value="ai"
              className="mt-4 flex-1 min-h-0 overflow-y-auto"
            >
              <div className="flex flex-col gap-3">
                <div className="rounded-xl border bg-card divide-y overflow-hidden">
                  {/* Provider & key */}
                  <div className="p-5 flex flex-col gap-4">
                    <div className="flex items-center justify-between gap-2">
                      <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                        {t("scenarios.aiSection.provider")}
                      </span>
                      {aiHasKey && (
                        <Badge
                          variant="secondary"
                          className="bg-success/15 text-success"
                        >
                          {t("scenarios.form.keySet")}
                        </Badge>
                      )}
                    </div>
                    <div className="grid grid-cols-2 gap-x-4 gap-y-4">
                      <div className="flex flex-col gap-1.5">
                        <FieldLabel>{t("scenarios.aiProvider")}</FieldLabel>
                        <Select
                          value={aiProvider}
                          onValueChange={(v) => {
                            const p = v as ScenarioAiProvider;
                            setAiProvider(p);
                            // Switch to that provider's default model unless the
                            // current one is already one of its suggestions.
                            if (!PROVIDER_META[p].models.includes(aiModel)) {
                              setAiModel(PROVIDER_META[p].models[0]);
                            }
                          }}
                        >
                          <SelectTrigger className="h-9">
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            {PROVIDERS.map((p) => (
                              <SelectItem key={p} value={p}>
                                {p.charAt(0).toUpperCase() + p.slice(1)}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      </div>
                      <div className="flex flex-col gap-1.5">
                        <FieldLabel>{t("scenarios.aiModel")}</FieldLabel>
                        <Input
                          value={aiModel}
                          onChange={(e) => setAiModel(e.target.value)}
                          list="ai-model-suggestions"
                          className="h-9"
                        />
                        <datalist id="ai-model-suggestions">
                          {PROVIDER_META[aiProvider].models.map((m) => (
                            <option key={m} value={m} />
                          ))}
                        </datalist>
                      </div>
                      {PROVIDER_META[aiProvider].needsKey ? (
                        <div className="flex flex-col gap-1.5">
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
                            className="h-9"
                          />
                        </div>
                      ) : (
                        <div className="flex flex-col gap-1.5">
                          <FieldLabel>{t("scenarios.aiApiKey")}</FieldLabel>
                          <div className="h-9 flex items-center text-xs text-muted-foreground rounded-md bg-muted/40 px-3">
                            {t("scenarios.aiNoKeyNote")}
                          </div>
                        </div>
                      )}
                      <div className="flex flex-col gap-1.5">
                        <FieldLabel>{t("scenarios.aiBaseUrl")}</FieldLabel>
                        <Input
                          value={aiBaseUrl}
                          onChange={(e) => setAiBaseUrl(e.target.value)}
                          placeholder={PROVIDER_META[aiProvider].baseUrl}
                          className="h-9"
                        />
                        <span className="text-[11px] text-muted-foreground">
                          {t("scenarios.aiBaseUrlHint")}
                        </span>
                      </div>
                    </div>
                  </div>

                  {/* Generation params */}
                  <div className="p-5 flex flex-col gap-4">
                    <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
                      {t("scenarios.aiSection.generation")}
                    </span>
                    <div className="grid grid-cols-2 gap-3">
                      <div className="flex flex-col gap-1.5">
                        <FieldLabel>{t("scenarios.aiMaxTokens")}</FieldLabel>
                        <Input
                          type="number"
                          min="1"
                          step="1"
                          value={aiMaxTokens}
                          onChange={(e) => setAiMaxTokens(e.target.value)}
                          className="h-9"
                        />
                      </div>
                      <div className="flex flex-col gap-1.5">
                        <FieldLabel>{t("scenarios.aiTemperature")}</FieldLabel>
                        <Input
                          type="number"
                          step="0.1"
                          min="0"
                          max="2"
                          value={aiTemperature}
                          onChange={(e) => setAiTemperature(e.target.value)}
                          className="h-9"
                        />
                      </div>
                    </div>
                  </div>

                  {/* Actions */}
                  <div className="px-5 py-3.5 flex items-center gap-2 bg-muted/20">
                    <Button
                      size="sm"
                      disabled={!aiModel.trim()}
                      onClick={() => void handleSaveAi()}
                    >
                      <LuSave className="size-3.5" /> {t("scenarios.save")}
                    </Button>
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={aiTesting || !aiModel.trim()}
                      onClick={() => void handleTestAi()}
                    >
                      <LuPlug className="size-3.5" />{" "}
                      {aiTesting
                        ? t("scenarios.aiTesting")
                        : t("scenarios.aiTest")}
                    </Button>
                    <div className="flex-1" />
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
                <p className="text-xs text-muted-foreground px-1">
                  {t("scenarios.aiHint")}
                </p>
              </div>
            </AnimatedTabsContent>
          </AnimatedTabs>
        </div>

        {/* Floating bulk-delete bar for the scenario table (Network-screen pattern). */}
        {isOpen && activeTab === "editor" && (
          <DataTableActionBar table={scenariosTable}>
            <DataTableActionBarSelection table={scenariosTable} />
            <DataTableActionBarAction
              tooltip={t("scenarios.bulkTest.tooltip")}
              onClick={() => setShowBulkTest(true)}
              size="icon"
            >
              <LuPlay />
            </DataTableActionBarAction>
            <DataTableActionBarAction
              tooltip={t("common.buttons.delete")}
              onClick={() => setShowBulkDeleteScenarios(true)}
              size="icon"
              variant="destructive"
              className="border-destructive bg-destructive/50 hover:bg-destructive/70"
            >
              <LuTrash2 />
            </DataTableActionBarAction>
          </DataTableActionBar>
        )}

        {/* Bulk test: pick one running profile, run the selected scenarios in order. */}
        <Dialog open={showBulkTest} onOpenChange={setShowBulkTest}>
          <DialogContent className="max-w-md">
            <DialogHeader>
              <DialogTitle>
                {t("scenarios.bulkTest.title", {
                  count: selectedScenarios.length,
                })}
              </DialogTitle>
              <DialogDescription>
                {t("scenarios.bulkTest.hint")}
              </DialogDescription>
            </DialogHeader>
            <div className="flex flex-col gap-3">
              <Select
                value={bulkTestProfileId}
                onValueChange={setBulkTestProfileId}
              >
                <SelectTrigger className="h-9">
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
              {runnableProfiles.length === 0 && (
                <p className="text-xs text-muted-foreground">
                  {t("scenarios.noRunningProfiles")}
                </p>
              )}
            </div>
            <DialogFooter>
              <Button
                size="sm"
                disabled={
                  isBulkTesting ||
                  !bulkTestProfileId ||
                  selectedScenarios.length === 0
                }
                onClick={() => void handleBulkTest()}
              >
                <LuPlay className="size-3.5" />{" "}
                {isBulkTesting
                  ? t("scenarios.bulkTest.running")
                  : t("scenarios.test")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        <DeleteConfirmationDialog
          isOpen={showBulkDeleteScenarios}
          onClose={() => setShowBulkDeleteScenarios(false)}
          onConfirm={handleBulkDeleteScenarios}
          isLoading={isBulkDeletingScenarios}
          title={t("scenarios.bulkDelete.title")}
          description={t("scenarios.bulkDelete.description", {
            count: selectedScenarios.length,
            names: selectedScenarios.map((s) => s.name).join(", "),
          })}
          confirmButtonText={t("scenarios.bulkDelete.confirmButton", {
            count: selectedScenarios.length,
          })}
        />

        {/* Floating bulk-delete bar for the schedule table. */}
        {isOpen && activeTab === "schedules" && (
          <DataTableActionBar table={schedulesTable}>
            <DataTableActionBarSelection table={schedulesTable} />
            <DataTableActionBarAction
              tooltip={t("common.buttons.delete")}
              onClick={() => setShowBulkDeleteSchedules(true)}
              size="icon"
              variant="destructive"
              className="border-destructive bg-destructive/50 hover:bg-destructive/70"
            >
              <LuTrash2 />
            </DataTableActionBarAction>
          </DataTableActionBar>
        )}

        <DeleteConfirmationDialog
          isOpen={showBulkDeleteSchedules}
          onClose={() => setShowBulkDeleteSchedules(false)}
          onConfirm={handleBulkDeleteSchedules}
          isLoading={isBulkDeletingSchedules}
          title={t("scenarios.bulkDeleteSchedules.title")}
          description={t("scenarios.bulkDeleteSchedules.description", {
            count: selectedSchedules.length,
            names: selectedSchedules.map((s) => s.name).join(", "),
          })}
          confirmButtonText={t("scenarios.bulkDelete.confirmButton", {
            count: selectedSchedules.length,
          })}
        />

        {/* Read-only scenario flow overview */}
        <Dialog
          open={overviewScenario !== null}
          onOpenChange={(o) => !o && setOverviewScenario(null)}
        >
          <DialogContent className="max-w-xl max-h-[85vh] flex flex-col">
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                <LuEye className="size-4 shrink-0" />
                {t("scenarios.overviewTitle", {
                  name: overviewScenario?.name ?? "",
                })}
              </DialogTitle>
              <DialogDescription className="sr-only">
                {t("scenarios.overview")}
              </DialogDescription>
            </DialogHeader>
            <div className="flex-1 min-h-0 overflow-y-auto pr-1 py-1">
              {overviewScenario && (
                <ScenarioFlow blocks={overviewScenario.blocks} />
              )}
            </div>
            <DialogFooter>
              <Button
                size="sm"
                variant="outline"
                onClick={() => setOverviewScenario(null)}
              >
                {t("common.buttons.close")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        {/* How-to-use guide */}
        <Dialog open={showGuide} onOpenChange={setShowGuide}>
          <DialogContent className="max-w-2xl max-h-[85vh] flex flex-col">
            <DialogHeader>
              <DialogTitle className="flex items-center gap-2">
                <LuBookOpen className="size-4 shrink-0" />
                {t("scenarios.guideDialog.title")}
              </DialogTitle>
              <DialogDescription>
                {t("scenarios.guideDialog.intro")}
              </DialogDescription>
            </DialogHeader>
            <div className="flex-1 min-h-0 overflow-y-auto pr-1 flex flex-col gap-4 text-sm">
              {(["usage", "blocks", "params"] as const).map((sec) => (
                <section key={sec} className="flex flex-col gap-1.5">
                  <h4 className="font-semibold">
                    {t(`scenarios.guideDialog.${sec}Heading`)}
                  </h4>
                  <p className="text-muted-foreground whitespace-pre-line leading-relaxed">
                    {t(`scenarios.guideDialog.${sec}Body`)}
                  </p>
                </section>
              ))}
            </div>
            <DialogFooter>
              <Button
                size="sm"
                variant="outline"
                onClick={() => setShowGuide(false)}
              >
                {t("common.buttons.close")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

        {/* Confirm enable/disable a schedule */}
        <Dialog
          open={pendingToggle !== null}
          onOpenChange={(o) => !o && setPendingToggle(null)}
        >
          <DialogContent className="max-w-md">
            <DialogHeader>
              <DialogTitle>{t("scenarios.confirmToggle.title")}</DialogTitle>
              <DialogDescription>
                {pendingToggle?.enabled
                  ? t("scenarios.confirmToggle.disableDesc", {
                      name: pendingToggle?.name ?? "",
                    })
                  : t("scenarios.confirmToggle.enableDesc", {
                      name: pendingToggle?.name ?? "",
                    })}
              </DialogDescription>
            </DialogHeader>
            <DialogFooter>
              <Button
                size="sm"
                variant="outline"
                disabled={isToggling}
                onClick={() => setPendingToggle(null)}
              >
                {t("common.buttons.cancel")}
              </Button>
              <Button
                size="sm"
                disabled={isToggling}
                onClick={() => void confirmToggle()}
              >
                {t("common.buttons.confirm")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>

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
