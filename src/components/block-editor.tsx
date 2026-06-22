"use client";

import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  LuChevronDown,
  LuChevronRight,
  LuGripVertical,
  LuPlus,
  LuTrash2,
} from "react-icons/lu";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Textarea } from "@/components/ui/textarea";
import type { ScenarioBlock, ScenarioOnError } from "@/types";

// Field def per block type. Keys MUST match what the Rust executor reads
// (see src-tauri/src/scenario/executor.rs run_leaf / run_condition).
interface Field {
  key: string;
  type: "text" | "number" | "textarea";
  placeholder?: string;
  /** i18n key under `scenarios.builder.ph.*` for prose placeholders. */
  placeholderKey?: string;
}

const FIELD_SCHEMA: Record<string, Field[]> = {
  open_url: [{ key: "url", type: "text", placeholder: "https://example.com" }],
  get_page_text: [{ key: "output_variable", type: "text" }],
  get_page_html: [{ key: "output_variable", type: "text" }],
  get_url: [],
  screenshot: [],
  refresh: [],
  go_back: [{ key: "steps", type: "number" }],
  go_forward: [{ key: "steps", type: "number" }],
  scroll: [
    { key: "distance_px", type: "number", placeholderKey: "scrollDistance" },
    { key: "duration_ms", type: "number", placeholder: "900" },
  ],
  find_elements: [{ key: "output_variable", type: "text" }],
  click: [{ key: "selector", type: "text", placeholder: ".btn" }],
  click_by_index: [{ key: "index", type: "number" }],
  type_text: [
    { key: "selector", type: "text", placeholder: "input[name=q]" },
    { key: "text", type: "textarea" },
  ],
  press_key: [
    { key: "key", type: "text", placeholder: "Enter" },
    { key: "modifiers", type: "text", placeholderKey: "pressKeyModifiers" },
  ],
  upload_file: [
    { key: "selector", type: "text", placeholder: "input[type=file]" },
    { key: "files", type: "textarea", placeholderKey: "uploadFiles" },
  ],
  new_tab: [{ key: "url", type: "text", placeholderKey: "newTabUrl" }],
  switch_tab: [{ key: "index", type: "number" }],
  close_tab: [{ key: "index", type: "number" }],
  list_tabs: [{ key: "output_variable", type: "text" }],
  run_js: [
    {
      key: "expression",
      type: "textarea",
      placeholder: "return document.title",
    },
    { key: "output_variable", type: "text" },
  ],
  post: [
    { key: "selector", type: "text" },
    { key: "text", type: "textarea" },
  ],
  reply: [
    { key: "selector", type: "text" },
    { key: "text", type: "textarea" },
  ],
  submit: [
    { key: "selector", type: "text" },
    { key: "text", type: "textarea" },
  ],
  wait: [{ key: "seconds", type: "number", placeholder: "1" }],
  wait_random: [
    { key: "min_s", type: "number", placeholder: "1" },
    { key: "max_s", type: "number", placeholder: "3" },
  ],
  set_variable: [
    { key: "name", type: "text" },
    { key: "value", type: "text" },
  ],
  log: [{ key: "message", type: "text" }],
  loop: [{ key: "count", type: "number", placeholder: "5" }],
  for_each: [{ key: "source", type: "text", placeholderKey: "forEachSource" }],
  condition: [
    { key: "variable", type: "text", placeholderKey: "conditionVariable" },
    { key: "equals", type: "text", placeholderKey: "conditionEquals" },
    { key: "less_than", type: "number", placeholderKey: "conditionLessThan" },
  ],
  break: [],
  continue: [],
  stop: [],
  pick_row: [
    { key: "dataset_id", type: "text", placeholder: "dataset id" },
    { key: "prefix", type: "text", placeholder: "row (optional)" },
    { key: "index", type: "number", placeholder: "row index (optional)" },
  ],
  load_dataset: [
    { key: "dataset_id", type: "text", placeholder: "dataset id" },
    { key: "output_variable", type: "text" },
  ],
  set_profile_tag: [
    { key: "tag", type: "text", placeholder: "tag value (e.g. {{row.email}})" },
  ],
};

// Add-menu groups → block types. `key` maps to scenarios.builder.groups.*.
export const TYPE_GROUPS: { key: string; types: string[] }[] = [
  {
    key: "navigate",
    types: ["open_url", "go_back", "go_forward", "refresh", "scroll"],
  },
  {
    key: "read",
    types: [
      "get_page_text",
      "get_page_html",
      "get_url",
      "find_elements",
      "screenshot",
    ],
  },
  {
    key: "interact",
    types: [
      "click",
      "click_by_index",
      "type_text",
      "press_key",
      "upload_file",
      "run_js",
    ],
  },
  {
    key: "tabs",
    types: ["new_tab", "switch_tab", "close_tab", "list_tabs"],
  },
  { key: "outbound", types: ["post", "reply", "submit"] },
  {
    key: "flow",
    types: ["loop", "for_each", "condition", "break", "continue", "stop"],
  },
  { key: "variables", types: ["set_variable", "log", "wait", "wait_random"] },
  { key: "data", types: ["pick_row", "load_dataset", "set_profile_tag"] },
  {
    key: "ai",
    types: [
      "ai_write",
      "ai_decide",
      "ai_check",
      "ai_extract",
      "ai_summarize",
      "ai_find_element",
    ],
  },
];

const AI_FIELDS: Field[] = [
  { key: "prompt", type: "textarea" },
  { key: "output_variable", type: "text" },
];

const OUTBOUND = new Set(["post", "reply", "submit"]);

// Condition operators — must match executor.rs eval_condition (`op` values).
const COND_OPS = [
  "equals",
  "not_equals",
  "less_than",
  "greater_than",
  "contains",
] as const;
type CondOp = (typeof COND_OPS)[number];

// Acronyms shown upper-cased in friendly block names.
const ACRONYMS = new Set(["url", "html", "js", "ai"]);

/** snake_case block type → friendly Title Case (e.g. open_url → "Open URL"). */
export function prettify(type: string): string {
  return type
    .split("_")
    .map((w) =>
      ACRONYMS.has(w)
        ? w.toUpperCase()
        : w.charAt(0).toUpperCase() + w.slice(1),
    )
    .join(" ");
}

// Friendlier field labels than the raw param key. Keys are technical (they map
// 1:1 to what the Rust executor reads), so the override stays close to the key
// but adds units/clarity; anything not listed falls back to prettify().
const FIELD_LABELS: Record<string, string> = {
  url: "URL",
  output_variable: "Save result to",
  steps: "Steps",
  distance_px: "Distance (px)",
  duration_ms: "Duration (ms)",
  selector: "CSS selector",
  index: "Index",
  text: "Text",
  key: "Key",
  modifiers: "Modifiers",
  files: "Files",
  dataset_id: "Dataset",
  prefix: "Prefix",
  expression: "JavaScript",
  seconds: "Seconds",
  min_s: "Min (s)",
  max_s: "Max (s)",
  name: "Name",
  value: "Value",
  message: "Message",
  count: "Repeat count",
  source: "List variable",
  prompt: "Prompt",
  variable: "Variable",
};
function fieldLabel(key: string): string {
  return FIELD_LABELS[key] ?? prettify(key);
}

/** Read the active condition operator + value from params (new `op`/`value`,
 *  falling back to the legacy `equals`/`less_than`/`greater_than` keys). */
function readCondition(block: ScenarioBlock): { op: CondOp; value: string } {
  const p = (block.params as Record<string, unknown>) ?? {};
  const str = (v: unknown) => (v === undefined || v === null ? "" : String(v));
  if (
    typeof p.op === "string" &&
    (COND_OPS as readonly string[]).includes(p.op)
  ) {
    return { op: p.op as CondOp, value: str(p.value) };
  }
  if (p.equals !== undefined) return { op: "equals", value: str(p.equals) };
  if (p.less_than !== undefined)
    return { op: "less_than", value: str(p.less_than) };
  if (p.greater_than !== undefined)
    return { op: "greater_than", value: str(p.greater_than) };
  return { op: "equals", value: "" };
}

/** Write a normalized condition (variable + op + value), dropping legacy keys. */
function writeCondition(
  block: ScenarioBlock,
  patch: { variable?: string; op?: CondOp; value?: string },
): ScenarioBlock {
  const cur = readCondition(block);
  const p = { ...((block.params as Record<string, unknown>) ?? {}) };
  delete p.equals;
  delete p.less_than;
  delete p.greater_than;
  const variable =
    patch.variable !== undefined
      ? patch.variable
      : ((p.variable as string) ?? "");
  p.variable = variable;
  p.op = patch.op ?? cur.op;
  p.value = patch.value !== undefined ? patch.value : cur.value;
  return { ...block, params: p };
}

/** One-line summary of a block's key params, shown when collapsed. */
export function summarize(block: ScenarioBlock): string {
  const p = (block.params as Record<string, unknown>) ?? {};
  if (block.type === "condition") {
    const c = readCondition(block);
    return `${p.variable ?? "?"} ${c.op} ${c.value}`.trim();
  }
  const order = [
    "url",
    "selector",
    "text",
    "key",
    "count",
    "source",
    "seconds",
    "expression",
    "message",
    "name",
    "index",
    "prompt",
    "output_variable",
  ];
  const parts: string[] = [];
  for (const k of order) {
    const v = p[k];
    if (v !== undefined && v !== null && v !== "") {
      parts.push(String(v));
      if (parts.length >= 2) break;
    }
  }
  const s = parts.join(" · ");
  return s.length > 64 ? `${s.slice(0, 64)}…` : s;
}

let blockSeq = 0;
/** Stable client-side id for new blocks so React keys survive reorder/remove. */
function blockId(): string {
  try {
    return crypto.randomUUID();
  } catch {
    blockSeq += 1;
    return `blk-${Date.now()}-${blockSeq}`;
  }
}

function isAi(type: string): boolean {
  return type.startsWith("ai_");
}
function fieldsFor(type: string): Field[] {
  if (isAi(type)) return AI_FIELDS;
  return FIELD_SCHEMA[type] ?? [];
}
export function hasChildren(type: string): boolean {
  return type === "loop" || type === "for_each" || type === "condition";
}

function getParam(block: ScenarioBlock, key: string): string {
  const p = block.params as Record<string, unknown> | undefined;
  const v = p?.[key];
  if (v === undefined || v === null) return "";
  return typeof v === "string" ? v : String(v);
}

function withParam(
  block: ScenarioBlock,
  key: string,
  value: string,
  numeric: boolean,
): ScenarioBlock {
  const p = { ...((block.params as Record<string, unknown>) ?? {}) };
  if (value === "") {
    delete p[key];
  } else {
    p[key] = numeric ? Number(value) : value;
  }
  return { ...block, params: p };
}

interface BlockEditorProps {
  blocks: ScenarioBlock[];
  onChange: (next: ScenarioBlock[]) => void;
  /** Custom-type input is offered only at the top level. */
  depth?: number;
}

export function BlockEditor({ blocks, onChange, depth = 0 }: BlockEditorProps) {
  const { t } = useTranslation();
  // Pointer-based drag-to-reorder (HTML5 DnD is swallowed by the Tauri webview).
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [overIndex, setOverIndex] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const dragFromRef = useRef<number | null>(null);
  const overRef = useRef<number | null>(null);

  const addBlock = (type: string) => {
    onChange([...blocks, { id: blockId(), type, params: {} }]);
  };
  const updateAt = (i: number, next: ScenarioBlock) => {
    const copy = blocks.slice();
    copy[i] = next;
    onChange(copy);
  };
  const removeAt = (i: number) => {
    onChange(blocks.filter((_, idx) => idx !== i));
  };
  // `to` is an insert-before index in 0..length.
  const reorder = (from: number, to: number) => {
    if (to === from || to === from + 1) return;
    const copy = blocks.slice();
    const [moved] = copy.splice(from, 1);
    copy.splice(to > from ? to - 1 : to, 0, moved);
    onChange(copy);
  };

  const beginDrag = (i: number) => {
    dragFromRef.current = i;
    overRef.current = i;
    setDragIndex(i);
    setOverIndex(i);
  };
  const moveDrag = (clientY: number) => {
    const c = listRef.current;
    if (!c) return;
    const rows = Array.from(
      c.querySelectorAll<HTMLElement>(":scope > [data-block-index]"),
    );
    let target = rows.length;
    for (let k = 0; k < rows.length; k++) {
      const r = rows[k].getBoundingClientRect();
      if (clientY < r.top + r.height / 2) {
        target = k;
        break;
      }
    }
    overRef.current = target;
    setOverIndex(target);
  };
  const endDrag = () => {
    if (dragFromRef.current !== null && overRef.current !== null) {
      reorder(dragFromRef.current, overRef.current);
    }
    dragFromRef.current = null;
    overRef.current = null;
    setDragIndex(null);
    setOverIndex(null);
  };

  return (
    <div ref={listRef} className="flex flex-col gap-1.5">
      {blocks.map((block, i) => (
        <BlockRow
          key={block.id || i}
          block={block}
          depth={depth}
          index={i}
          isDragging={dragIndex === i}
          isDragOver={overIndex === i && dragIndex !== null && dragIndex !== i}
          onChange={(b) => updateAt(i, b)}
          onRemove={() => removeAt(i)}
          onBeginDrag={() => beginDrag(i)}
          onMoveDrag={moveDrag}
          onEndDrag={endDrag}
        />
      ))}

      <Select value="" onValueChange={addBlock}>
        <SelectTrigger className="h-8 w-44 text-xs border-dashed text-muted-foreground hover:text-foreground hover:border-solid">
          <span className="flex items-center gap-1">
            <LuPlus className="size-3.5" /> {t("scenarios.builder.addBlock")}
          </span>
        </SelectTrigger>
        <SelectContent>
          {TYPE_GROUPS.map((g) => (
            <SelectGroup key={g.key}>
              <SelectLabel>
                {t(`scenarios.builder.groups.${g.key}`)}
              </SelectLabel>
              {g.types.map((ty) => (
                <SelectItem key={ty} value={ty}>
                  <span className="flex items-baseline gap-2">
                    <span>{prettify(ty)}</span>
                    <span className="text-[10px] font-mono text-muted-foreground">
                      {ty}
                    </span>
                  </span>
                </SelectItem>
              ))}
            </SelectGroup>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}

interface BlockRowProps {
  block: ScenarioBlock;
  depth: number;
  index: number;
  isDragging: boolean;
  isDragOver: boolean;
  onChange: (b: ScenarioBlock) => void;
  onRemove: () => void;
  onBeginDrag: () => void;
  onMoveDrag: (clientY: number) => void;
  onEndDrag: () => void;
}

function BlockRow({
  block,
  depth,
  index,
  isDragging,
  isDragOver,
  onChange,
  onRemove,
  onBeginDrag,
  onMoveDrag,
  onEndDrag,
}: BlockRowProps) {
  const { t } = useTranslation();
  const fields = fieldsFor(block.type);
  // Collapsed by default — long scenarios stay scannable; expand to edit.
  const [expanded, setExpanded] = useState(false);
  const draggingRef = useRef(false);
  const nested = hasChildren(block.type);
  const isCondition = block.type === "condition";
  const summary = summarize(block);

  return (
    <div
      data-block-index={index}
      className={`rounded-md border bg-card transition-all ${
        block.disabled ? "opacity-60" : ""
      } ${isDragging ? "opacity-40" : ""} ${
        isDragOver ? "ring-2 ring-primary border-primary" : ""
      }`}
    >
      {/* Header — single dense line: chevron · type · label · actions */}
      <div className="flex items-center gap-1 pl-1 pr-1.5 h-9">
        <button
          type="button"
          onClick={() => setExpanded((v) => !v)}
          className="size-5 grid place-items-center text-muted-foreground hover:text-foreground shrink-0"
          aria-label={
            expanded
              ? t("scenarios.builder.collapse")
              : t("scenarios.builder.expand")
          }
        >
          {expanded ? (
            <LuChevronDown className="size-3.5" />
          ) : (
            <LuChevronRight className="size-3.5" />
          )}
        </button>
        <span
          title={block.type}
          className="text-[11px] font-semibold px-1.5 py-0.5 rounded bg-muted text-foreground shrink-0"
        >
          {prettify(block.type)}
        </span>
        {expanded ? (
          <Input
            value={block.label ?? ""}
            onChange={(e) =>
              onChange({ ...block, label: e.target.value || undefined })
            }
            placeholder={t("scenarios.builder.label")}
            className="h-7 text-xs flex-1 min-w-0 border-0 bg-transparent shadow-none px-1.5 focus-visible:bg-muted/50 focus-visible:ring-0"
          />
        ) : (
          // Collapsed: show label + a compact param summary instead of the editor.
          <button
            type="button"
            onClick={() => setExpanded(true)}
            className="flex items-baseline gap-2 flex-1 min-w-0 text-left px-1.5"
          >
            {block.label && (
              <span className="text-xs truncate shrink-0">{block.label}</span>
            )}
            <span className="text-[11px] text-muted-foreground font-mono truncate">
              {summary}
            </span>
          </button>
        )}
        {block.disabled && (
          <span className="text-[10px] uppercase tracking-wide text-muted-foreground shrink-0 px-1">
            {t("scenarios.builder.disabled")}
          </span>
        )}
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7 text-destructive hover:text-destructive"
          onClick={onRemove}
        >
          <LuTrash2 className="size-3.5" />
        </Button>
        {/* Drag handle — press & drag to reorder blocks (pointer-based). */}
        <span
          onPointerDown={(e) => {
            e.preventDefault();
            draggingRef.current = true;
            e.currentTarget.setPointerCapture(e.pointerId);
            onBeginDrag();
          }}
          onPointerMove={(e) => {
            if (draggingRef.current) onMoveDrag(e.clientY);
          }}
          onPointerUp={(e) => {
            if (!draggingRef.current) return;
            draggingRef.current = false;
            try {
              e.currentTarget.releasePointerCapture(e.pointerId);
            } catch {
              // ignore — capture may already be released
            }
            onEndDrag();
          }}
          title={t("scenarios.builder.drag")}
          className="size-7 grid place-items-center text-muted-foreground hover:text-foreground shrink-0 cursor-grab active:cursor-grabbing touch-none"
        >
          <LuGripVertical className="size-3.5" />
        </span>
      </div>

      {expanded && (
        <div className="px-3 pb-3 pt-2.5 flex flex-col gap-3 border-t bg-muted/20">
          {/* What this block does — shared source with the in-app guide. */}
          {t(`scenarios.builder.blockDocs.${block.type}`, {
            defaultValue: "",
          }) && (
            <p className="text-[11px] leading-relaxed text-muted-foreground">
              {t(`scenarios.builder.blockDocs.${block.type}`)}
            </p>
          )}
          {isCondition && (
            <div className="flex flex-col gap-1.5">
              <div className="flex items-center gap-2">
                <span className="text-[11px] text-muted-foreground w-28 shrink-0 text-right">
                  {fieldLabel("variable")}
                </span>
                <Input
                  value={getParam(block, "variable")}
                  onChange={(e) =>
                    onChange(
                      writeCondition(block, { variable: e.target.value }),
                    )
                  }
                  placeholder={t("scenarios.builder.ph.conditionVariable")}
                  className="h-7 text-xs flex-1 min-w-0"
                />
              </div>
              <div className="flex items-center gap-2">
                <span className="text-[11px] font-mono text-muted-foreground w-28 shrink-0 text-right">
                  {t("scenarios.builder.operator")}
                </span>
                <Select
                  value={readCondition(block).op}
                  onValueChange={(v) =>
                    onChange(writeCondition(block, { op: v as CondOp }))
                  }
                >
                  <SelectTrigger className="h-7 text-xs w-40">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {COND_OPS.map((op) => (
                      <SelectItem key={op} value={op}>
                        {t(`scenarios.builder.condOps.${op}`)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              <div className="flex items-center gap-2">
                <span className="text-[11px] text-muted-foreground w-28 shrink-0 text-right">
                  {fieldLabel("value")}
                </span>
                <Input
                  value={readCondition(block).value}
                  onChange={(e) =>
                    onChange(writeCondition(block, { value: e.target.value }))
                  }
                  placeholder={t("scenarios.builder.ph.conditionEquals")}
                  className="h-7 text-xs flex-1 min-w-0"
                />
              </div>
            </div>
          )}
          {!isCondition && fields.length > 0 && (
            <div className="flex flex-col gap-2">
              {fields.map((f) => {
                const placeholder = f.placeholderKey
                  ? t(`scenarios.builder.ph.${f.placeholderKey}`)
                  : f.placeholder;
                return f.type === "textarea" ? (
                  <div key={f.key} className="flex flex-col gap-1">
                    <span className="text-[11px] font-medium text-foreground/80">
                      {fieldLabel(f.key)}
                    </span>
                    <Textarea
                      value={getParam(block, f.key)}
                      onChange={(e) =>
                        onChange(withParam(block, f.key, e.target.value, false))
                      }
                      placeholder={placeholder}
                      spellCheck={false}
                      className="text-xs min-h-16 resize-y font-mono"
                    />
                  </div>
                ) : (
                  <div key={f.key} className="flex items-center gap-2">
                    <span className="text-[11px] font-medium text-foreground/80 w-28 shrink-0 text-right">
                      {fieldLabel(f.key)}
                    </span>
                    <Input
                      type={f.type === "number" ? "number" : "text"}
                      value={getParam(block, f.key)}
                      onChange={(e) =>
                        onChange(
                          withParam(
                            block,
                            f.key,
                            e.target.value,
                            f.type === "number",
                          ),
                        )
                      }
                      placeholder={placeholder}
                      className="h-7 text-xs flex-1 min-w-0"
                    />
                  </div>
                );
              })}
            </div>
          )}
          {!isCondition && !nested && fields.length === 0 && (
            <p className="text-[11px] italic text-muted-foreground">
              {t("scenarios.builder.noParams")}
            </p>
          )}

          {/* Meta footer — compact options strip */}
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-[11px] text-muted-foreground border-t border-border/60 pt-2.5">
            {isAi(block.type) && (
              <span className="flex items-center gap-1.5">
                <Checkbox
                  checked={block.ai_enabled ?? false}
                  onCheckedChange={(c) =>
                    onChange({ ...block, ai_enabled: c === true })
                  }
                />
                {t("scenarios.builder.aiEnabled")}
              </span>
            )}
            {OUTBOUND.has(block.type) && (
              <span className="flex items-center gap-1.5">
                <Checkbox
                  checked={block.dry_run ?? false}
                  onCheckedChange={(c) =>
                    onChange({ ...block, dry_run: c === true })
                  }
                />
                {t("scenarios.builder.dryRun")}
              </span>
            )}
            <span className="flex items-center gap-1.5">
              <Checkbox
                checked={block.disabled ?? false}
                onCheckedChange={(c) =>
                  onChange({ ...block, disabled: c === true })
                }
              />
              {t("scenarios.builder.disabled")}
            </span>
            <span className="flex items-center gap-1.5 ml-auto">
              {t("scenarios.builder.onError")}
              <Select
                value={block.on_error ?? "inherit"}
                onValueChange={(v) =>
                  onChange({
                    ...block,
                    on_error:
                      v === "inherit" ? undefined : (v as ScenarioOnError),
                  })
                }
              >
                <SelectTrigger className="h-6 w-24 text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="inherit">
                    {t("scenarios.builder.onErrorOpts.inherit")}
                  </SelectItem>
                  <SelectItem value="stop">
                    {t("scenarios.builder.onErrorOpts.stop")}
                  </SelectItem>
                  <SelectItem value="skip">
                    {t("scenarios.builder.onErrorOpts.skip")}
                  </SelectItem>
                  <SelectItem value="retry">
                    {t("scenarios.builder.onErrorOpts.retry")}
                  </SelectItem>
                </SelectContent>
              </Select>
            </span>
          </div>

          {nested && (
            <div className="mt-0.5 pl-3 border-l-2 border-border flex flex-col gap-1">
              <span className="text-[11px] font-medium text-muted-foreground">
                {t("scenarios.builder.children")}
              </span>
              <BlockEditor
                blocks={block.children ?? []}
                onChange={(children) => onChange({ ...block, children })}
                depth={depth + 1}
              />
              {block.type === "condition" && (
                <>
                  <span className="text-[11px] font-medium text-muted-foreground mt-1">
                    {t("scenarios.builder.elseBranch")}
                  </span>
                  <BlockEditor
                    blocks={block.branch_else ?? []}
                    onChange={(branch_else) =>
                      onChange({ ...block, branch_else })
                    }
                    depth={depth + 1}
                  />
                </>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
