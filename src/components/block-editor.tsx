"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  LuChevronDown,
  LuChevronRight,
  LuChevronUp,
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
};

// Add-menu groups → block types. `key` maps to scenarios.builder.groups.*.
const TYPE_GROUPS: { key: string; types: string[] }[] = [
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
    types: ["click", "click_by_index", "type_text", "run_js"],
  },
  { key: "outbound", types: ["post", "reply", "submit"] },
  {
    key: "flow",
    types: ["loop", "for_each", "condition", "break", "continue", "stop"],
  },
  { key: "variables", types: ["set_variable", "log", "wait", "wait_random"] },
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
function hasChildren(type: string): boolean {
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
  const move = (i: number, dir: -1 | 1) => {
    const j = i + dir;
    if (j < 0 || j >= blocks.length) return;
    const copy = blocks.slice();
    [copy[i], copy[j]] = [copy[j], copy[i]];
    onChange(copy);
  };

  return (
    <div className="flex flex-col gap-1.5">
      {blocks.map((block, i) => (
        <BlockRow
          key={block.id ?? i}
          block={block}
          depth={depth}
          isFirst={i === 0}
          isLast={i === blocks.length - 1}
          onChange={(b) => updateAt(i, b)}
          onRemove={() => removeAt(i)}
          onMoveUp={() => move(i, -1)}
          onMoveDown={() => move(i, 1)}
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
                  {ty}
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
  isFirst: boolean;
  isLast: boolean;
  onChange: (b: ScenarioBlock) => void;
  onRemove: () => void;
  onMoveUp: () => void;
  onMoveDown: () => void;
}

function BlockRow({
  block,
  depth,
  isFirst,
  isLast,
  onChange,
  onRemove,
  onMoveUp,
  onMoveDown,
}: BlockRowProps) {
  const { t } = useTranslation();
  const fields = fieldsFor(block.type);
  const [expanded, setExpanded] = useState(true);
  const nested = hasChildren(block.type);

  return (
    <div
      className={`rounded-md border bg-card transition-opacity ${
        block.disabled ? "opacity-60" : ""
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
        <span className="text-[11px] font-mono font-semibold px-1.5 py-0.5 rounded bg-muted text-foreground shrink-0">
          {block.type}
        </span>
        <Input
          value={block.label ?? ""}
          onChange={(e) =>
            onChange({ ...block, label: e.target.value || undefined })
          }
          placeholder={t("scenarios.builder.label")}
          className="h-7 text-xs flex-1 min-w-0 border-0 bg-transparent shadow-none px-1.5 focus-visible:bg-muted/50 focus-visible:ring-0"
        />
        {block.disabled && (
          <span className="text-[10px] uppercase tracking-wide text-muted-foreground shrink-0 px-1">
            {t("scenarios.builder.disabled")}
          </span>
        )}
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7"
          disabled={isFirst}
          onClick={onMoveUp}
        >
          <LuChevronUp className="size-3.5" />
        </Button>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7"
          disabled={isLast}
          onClick={onMoveDown}
        >
          <LuChevronDown className="size-3.5" />
        </Button>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7 text-destructive hover:text-destructive"
          onClick={onRemove}
        >
          <LuTrash2 className="size-3.5" />
        </Button>
      </div>

      {expanded && (
        <div className="px-2 pb-2 pt-2 flex flex-col gap-2 border-t">
          {fields.length > 0 && (
            <div className="flex flex-col gap-1.5">
              {fields.map((f) => {
                const placeholder = f.placeholderKey
                  ? t(`scenarios.builder.ph.${f.placeholderKey}`)
                  : f.placeholder;
                return f.type === "textarea" ? (
                  <div key={f.key} className="flex flex-col gap-0.5">
                    <span className="text-[11px] font-mono text-muted-foreground">
                      {f.key}
                    </span>
                    <Textarea
                      value={getParam(block, f.key)}
                      onChange={(e) =>
                        onChange(withParam(block, f.key, e.target.value, false))
                      }
                      placeholder={placeholder}
                      spellCheck={false}
                      className="text-xs min-h-16 resize-y"
                    />
                  </div>
                ) : (
                  <div key={f.key} className="flex items-center gap-2">
                    <span className="text-[11px] font-mono text-muted-foreground w-28 shrink-0 text-right">
                      {f.key}
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

          {/* Meta footer — compact options strip */}
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-[11px] text-muted-foreground">
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
