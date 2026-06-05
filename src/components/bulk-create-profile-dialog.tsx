"use client";

import { invoke } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { LoadingButton } from "@/components/loading-button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
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
import { useBrowserDownload } from "@/hooks/use-browser-download";
import { translateBackendError } from "@/lib/backend-errors";
import { showErrorToast, showSuccessToast } from "@/lib/toast-utils";
import type {
  BrowserReleaseTypes,
  CamoufoxConfig,
  CamoufoxOS,
  ParsedProxyLine,
  ProxyParseResult,
  WayfernConfig,
  WayfernOS,
} from "@/types";
import { RippleButton } from "./ui/ripple";

type BrowserTypeString = "camoufox" | "wayfern";

const MAX_BULK = 500;

const getCurrentOS = (): CamoufoxOS => {
  if (typeof navigator === "undefined") return "linux";
  const platform = navigator.platform.toLowerCase();
  if (platform.includes("win")) return "windows";
  if (platform.includes("mac")) return "macos";
  return "linux";
};

interface BulkCreateProfileDialogProps {
  isOpen: boolean;
  onClose: () => void;
  selectedGroupId?: string;
}

interface BulkCreateResult {
  created_count: number;
  errors: string[];
}

export function BulkCreateProfileDialog({
  isOpen,
  onClose,
  selectedGroupId,
}: BulkCreateProfileDialogProps) {
  const { t } = useTranslation();
  const [selectedBrowser, setSelectedBrowser] =
    useState<BrowserTypeString>("camoufox");
  // Empty by default: an empty prefix falls back to the browser's name
  // (e.g. "Camoufox 1", "Wayfern 1").
  const [namePrefix, setNamePrefix] = useState("");
  // Raw text so the field can be cleared while editing; the numeric `count`
  // below is derived and clamped.
  const [countInput, setCountInput] = useState("1");
  const [proxyText, setProxyText] = useState("");
  const [validProxies, setValidProxies] = useState<ParsedProxyLine[]>([]);
  const [invalidLineCount, setInvalidLineCount] = useState(0);
  const [releaseTypes, setReleaseTypes] = useState<BrowserReleaseTypes>();
  const [isCreating, setIsCreating] = useState(false);
  // True once the user edits the count by hand, so re-parsing proxies no longer
  // overrides their chosen value.
  const countTouchedRef = useRef(false);
  // Effective profile count: empty/invalid input resolves to 0 (submit disabled).
  const count = Math.min(
    Math.max(Number.parseInt(countInput, 10) || 0, 0),
    MAX_BULK,
  );

  const {
    downloadBrowser,
    loadDownloadedVersions,
    isVersionDownloaded,
    isBrowserDownloading,
    downloadedVersionsMap,
  } = useBrowserDownload();

  const resetState = useCallback(() => {
    setSelectedBrowser("camoufox");
    setNamePrefix("");
    setCountInput("1");
    setProxyText("");
    setValidProxies([]);
    setInvalidLineCount(0);
    setIsCreating(false);
    countTouchedRef.current = false;
  }, []);

  // Load the available versions for both browsers when the dialog opens.
  useEffect(() => {
    if (!isOpen) return;
    void loadDownloadedVersions("camoufox");
    void loadDownloadedVersions("wayfern");
  }, [isOpen, loadDownloadedVersions]);

  // Resolve the best release type whenever the selected browser changes.
  useEffect(() => {
    if (!isOpen) return;
    let cancelled = false;
    setReleaseTypes(undefined);
    void invoke<BrowserReleaseTypes>("get_browser_release_types", {
      browserStr: selectedBrowser,
    })
      .then((raw) => {
        if (cancelled) return;
        const filtered: BrowserReleaseTypes = {};
        if (raw.stable) filtered.stable = raw.stable;
        setReleaseTypes(filtered);
      })
      .catch(() => {
        if (!cancelled) setReleaseTypes({});
      });
    return () => {
      cancelled = true;
    };
  }, [isOpen, selectedBrowser]);

  // Parse the pasted proxies (debounced). Only cleanly-parsed lines are usable;
  // ambiguous and invalid lines are counted as skipped.
  useEffect(() => {
    if (!isOpen) return;
    const handle = setTimeout(() => {
      const trimmed = proxyText.trim();
      if (!trimmed) {
        setValidProxies([]);
        setInvalidLineCount(0);
        return;
      }
      void invoke<ProxyParseResult[]>("parse_txt_proxies", {
        content: proxyText,
      })
        .then((results) => {
          const valid: ParsedProxyLine[] = [];
          let invalid = 0;
          for (const r of results) {
            if (r.status === "parsed") valid.push(r);
            else invalid += 1;
          }
          setValidProxies(valid);
          setInvalidLineCount(invalid);
          if (!countTouchedRef.current && valid.length > 0) {
            setCountInput(String(Math.min(valid.length, MAX_BULK)));
          }
        })
        .catch(() => {
          setValidProxies([]);
          setInvalidLineCount(0);
        });
    }, 350);
    return () => {
      clearTimeout(handle);
    };
  }, [isOpen, proxyText]);

  // The latest stable version that is actually downloaded for this browser.
  const creatableVersion = useMemo(() => {
    const stable = releaseTypes?.stable;
    if (stable && isVersionDownloaded(stable)) {
      return { version: stable, releaseType: "stable" as const };
    }
    const downloaded = downloadedVersionsMap[selectedBrowser] ?? [];
    if (downloaded.length > 0) {
      return { version: downloaded[0], releaseType: "stable" as const };
    }
    return null;
  }, [
    releaseTypes,
    isVersionDownloaded,
    downloadedVersionsMap,
    selectedBrowser,
  ]);

  const needsDownload = creatableVersion === null;
  const isDownloading = isBrowserDownloading(selectedBrowser);
  // Display name of the selected browser, used as the prefix fallback.
  const browserLabel = selectedBrowser === "camoufox" ? "Camoufox" : "Wayfern";
  const effectivePrefix = namePrefix.trim() || browserLabel;

  const handleDownload = useCallback(async () => {
    const stable = releaseTypes?.stable;
    if (!stable) return;
    try {
      await downloadBrowser(selectedBrowser, stable);
    } catch (error) {
      showErrorToast(
        t("bulkCreate.downloadFailed", {
          error: translateBackendError(t, error),
        }),
      );
    }
  }, [releaseTypes, selectedBrowser, downloadBrowser, t]);

  const handleClose = useCallback(() => {
    resetState();
    onClose();
  }, [resetState, onClose]);

  const handleCreate = useCallback(async () => {
    if (!creatableVersion || count < 1) return;
    setIsCreating(true);

    const camoufoxConfig: CamoufoxConfig | undefined =
      selectedBrowser === "camoufox"
        ? { geoip: true, os: getCurrentOS() }
        : undefined;
    const wayfernConfig: WayfernConfig | undefined =
      selectedBrowser === "wayfern"
        ? { os: getCurrentOS() as WayfernOS }
        : undefined;

    try {
      const result = await invoke<BulkCreateResult>(
        "create_browser_profiles_bulk",
        {
          count,
          browserStr: selectedBrowser,
          version: creatableVersion.version,
          releaseType: creatableVersion.releaseType,
          proxies: validProxies.slice(0, count),
          namePrefix: effectivePrefix,
          groupId:
            selectedGroupId && selectedGroupId !== "__all__"
              ? selectedGroupId
              : undefined,
          camoufoxConfig,
          wayfernConfig,
        },
      );

      await emit("stored-proxies-changed");

      if (result.created_count > 0) {
        showSuccessToast(
          t("bulkCreate.successToast", { count: result.created_count }),
        );
      }
      if (result.errors.length > 0) {
        showErrorToast(
          t("bulkCreate.partialErrors", { count: result.errors.length }),
        );
      }
      handleClose();
    } catch (error) {
      showErrorToast(
        t("bulkCreate.failed", { error: translateBackendError(t, error) }),
      );
    } finally {
      setIsCreating(false);
    }
  }, [
    creatableVersion,
    count,
    selectedBrowser,
    validProxies,
    effectivePrefix,
    selectedGroupId,
    handleClose,
    t,
  ]);

  const assignedProxies = Math.min(validProxies.length, count);

  return (
    <Dialog open={isOpen} onOpenChange={handleClose}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>{t("bulkCreate.title")}</DialogTitle>
          <DialogDescription>{t("bulkCreate.description")}</DialogDescription>
        </DialogHeader>

        <div className="space-y-4">
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-2">
              <Label htmlFor="bulk-browser">{t("bulkCreate.browser")}</Label>
              <Select
                value={selectedBrowser}
                onValueChange={(v) => {
                  setSelectedBrowser(v as BrowserTypeString);
                }}
              >
                <SelectTrigger id="bulk-browser">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="camoufox">Camoufox</SelectItem>
                  <SelectItem value="wayfern">Wayfern</SelectItem>
                </SelectContent>
              </Select>
            </div>

            <div className="space-y-2">
              <Label htmlFor="bulk-count">{t("bulkCreate.count")}</Label>
              <Input
                id="bulk-count"
                type="text"
                inputMode="numeric"
                value={countInput}
                onChange={(e) => {
                  countTouchedRef.current = true;
                  // Digits only; allow empty while editing.
                  setCountInput(e.target.value.replace(/[^0-9]/g, ""));
                }}
                onBlur={() => {
                  // Clamp to [1, MAX_BULK] when the field loses focus.
                  const n = Number.parseInt(countInput, 10);
                  if (Number.isNaN(n) || n < 1) {
                    setCountInput("1");
                  } else if (n > MAX_BULK) {
                    setCountInput(String(MAX_BULK));
                  }
                }}
              />
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="bulk-prefix">{t("bulkCreate.namePrefix")}</Label>
            <Input
              id="bulk-prefix"
              placeholder={browserLabel}
              value={namePrefix}
              onChange={(e) => {
                setNamePrefix(e.target.value);
              }}
            />
            <p className="text-xs text-muted-foreground">
              {t("bulkCreate.namePrefixHint", { prefix: effectivePrefix })}
            </p>
          </div>

          <div className="space-y-2">
            <Label htmlFor="bulk-proxies">{t("bulkCreate.proxies")}</Label>
            <Textarea
              id="bulk-proxies"
              className="h-[140px] font-mono text-xs"
              placeholder={t("bulkCreate.proxiesPlaceholder")}
              value={proxyText}
              onChange={(e) => {
                setProxyText(e.target.value);
              }}
            />
            <p className="text-xs text-muted-foreground">
              {t("bulkCreate.proxiesSummary", {
                valid: validProxies.length,
                invalid: invalidLineCount,
              })}
            </p>
          </div>

          {needsDownload ? (
            <div className="rounded-md border border-warning/50 bg-warning/10 p-3 space-y-2">
              <p className="text-xs text-warning-foreground">
                {t("bulkCreate.needsDownload")}
              </p>
              <LoadingButton
                size="sm"
                isLoading={isDownloading}
                disabled={!releaseTypes?.stable}
                onClick={() => void handleDownload()}
              >
                {t("common.buttons.download")}
              </LoadingButton>
            </div>
          ) : (
            <p className="text-xs text-muted-foreground">
              {t("bulkCreate.summary", {
                count,
                assigned: assignedProxies,
              })}
            </p>
          )}
        </div>

        <DialogFooter>
          <RippleButton variant="outline" onClick={handleClose}>
            {t("common.buttons.cancel")}
          </RippleButton>
          <LoadingButton
            isLoading={isCreating}
            disabled={needsDownload || count < 1}
            onClick={() => void handleCreate()}
          >
            {t("bulkCreate.createButton", { count })}
          </LoadingButton>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
