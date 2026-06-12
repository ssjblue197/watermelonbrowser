"use client";

import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ProBadge } from "@/components/ui/pro-badge";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import type { CloakConfig, CloakOS } from "@/types";

interface CloakConfigFormProps {
  config: CloakConfig;
  onConfigChange: (key: keyof CloakConfig, value: unknown) => void;
  className?: string;
  isCreating?: boolean;
  readOnly?: boolean;
  crossOsUnlocked?: boolean;
}

const getCurrentOS = (): CloakOS => {
  if (typeof navigator === "undefined") return "linux";
  const platform = navigator.platform.toLowerCase();
  if (platform.includes("win")) return "windows";
  if (platform.includes("mac")) return "macos";
  return "linux";
};

const osLabels: Record<CloakOS, string> = {
  windows: "Windows",
  macos: "macOS",
  linux: "Linux",
};

/** Cloak identity is a numeric seed in [10000, 99999]. */
const randomSeed = (): number => 10000 + Math.floor(Math.random() * 90000);

export function CloakConfigForm({
  config,
  onConfigChange,
  className = "",
  isCreating = false,
  readOnly = false,
  crossOsUnlocked = false,
}: CloakConfigFormProps) {
  const { t } = useTranslation();
  const [currentOS] = useState<CloakOS>(getCurrentOS);
  const selectedOS = config.os ?? currentOS;

  // Seed a value on first render of the create flow so the profile has a stable
  // identity even if the user never touches the field.
  useEffect(() => {
    if (isCreating && config.seed === undefined) {
      onConfigChange("seed", randomSeed());
    }
  }, [isCreating, config.seed, onConfigChange]);

  const randomizeOnLaunch = config.randomize_seed_on_launch ?? false;

  return (
    <div className={`space-y-6 ${className}`}>
      {/* Operating System */}
      <div className="space-y-3">
        <Label>{t("fingerprint.osLabel")}</Label>
        <Select
          value={selectedOS}
          onValueChange={(value: CloakOS) => onConfigChange("os", value)}
          disabled={readOnly}
        >
          <SelectTrigger>
            <SelectValue placeholder={t("fingerprint.selectOSPlaceholder")} />
          </SelectTrigger>
          <SelectContent>
            {(["windows", "macos", "linux"] as CloakOS[]).map((os) => {
              const isDisabled = os !== currentOS && !crossOsUnlocked;
              return (
                <SelectItem key={os} value={os} disabled={isDisabled}>
                  <span className="flex items-center gap-2">
                    {osLabels[os]}
                    {isDisabled && <ProBadge />}
                  </span>
                </SelectItem>
              );
            })}
          </SelectContent>
        </Select>
        {selectedOS !== currentOS && crossOsUnlocked && (
          <Alert className="mt-2">
            <AlertDescription>
              {t("fingerprint.crossOsWarning")}
            </AlertDescription>
          </Alert>
        )}
      </div>

      {/* Auto location & WebRTC from the proxy exit IP */}
      <div className="flex items-center gap-x-2">
        <Checkbox
          id="cloak-geoip"
          checked={config.geoip !== false}
          onCheckedChange={(checked) =>
            onConfigChange("geoip", checked === true)
          }
          disabled={readOnly}
        />
        <Label htmlFor="cloak-geoip">
          {t("fingerprint.autoLocationDescription")}
        </Label>
      </div>

      {/* Seed */}
      <div className="space-y-3 p-4 border rounded-lg bg-muted/30">
        <Label htmlFor="cloak-seed" className="font-medium">
          {t("cloak.seed")}
        </Label>
        <div className="flex items-center gap-2">
          <Input
            id="cloak-seed"
            type="number"
            min={10000}
            max={99999}
            value={config.seed ?? ""}
            onChange={(e) =>
              onConfigChange(
                "seed",
                e.target.value ? parseInt(e.target.value, 10) : undefined,
              )
            }
            placeholder="42069"
            disabled={readOnly || randomizeOnLaunch}
          />
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={readOnly || randomizeOnLaunch}
            onClick={() => onConfigChange("seed", randomSeed())}
          >
            {t("cloak.randomize")}
          </Button>
        </div>
        <p className="text-sm text-muted-foreground">
          {t("cloak.seedDescription")}
        </p>
        <div className="flex items-center gap-x-2 pt-1">
          <Checkbox
            id="cloak-randomize-on-launch"
            checked={randomizeOnLaunch}
            onCheckedChange={(checked) =>
              onConfigChange("randomize_seed_on_launch", checked)
            }
            disabled={readOnly}
          />
          <Label htmlFor="cloak-randomize-on-launch">
            {t("cloak.randomizeOnLaunch")}
          </Label>
        </div>
      </div>

      {/* Timezone + Locale */}
      <div className="grid grid-cols-2 gap-4">
        <div className="space-y-2">
          <Label htmlFor="cloak-timezone">
            {t("fingerprint.timezoneIana")}
          </Label>
          <Input
            id="cloak-timezone"
            value={config.timezone ?? ""}
            onChange={(e) =>
              onConfigChange("timezone", e.target.value || undefined)
            }
            placeholder={t("common.placeholders.example", {
              value: "America/New_York",
            })}
            disabled={readOnly}
          />
        </div>
        <div className="space-y-2">
          <Label htmlFor="cloak-locale">
            {t("fingerprint.primaryLanguage")}
          </Label>
          <Input
            id="cloak-locale"
            value={config.locale ?? ""}
            onChange={(e) =>
              onConfigChange("locale", e.target.value || undefined)
            }
            placeholder={t("common.placeholders.example", { value: "en-US" })}
            disabled={readOnly}
          />
        </div>
      </div>

      {/* Screen */}
      <div className="grid grid-cols-2 gap-4">
        <div className="space-y-2">
          <Label htmlFor="cloak-screen-width">
            {t("fingerprint.screenWidth")}
          </Label>
          <Input
            id="cloak-screen-width"
            type="number"
            value={config.screen_width ?? ""}
            onChange={(e) =>
              onConfigChange(
                "screen_width",
                e.target.value ? parseInt(e.target.value, 10) : undefined,
              )
            }
            placeholder={t("common.placeholders.example", { value: "1920" })}
            disabled={readOnly}
          />
        </div>
        <div className="space-y-2">
          <Label htmlFor="cloak-screen-height">
            {t("fingerprint.screenHeight")}
          </Label>
          <Input
            id="cloak-screen-height"
            type="number"
            value={config.screen_height ?? ""}
            onChange={(e) =>
              onConfigChange(
                "screen_height",
                e.target.value ? parseInt(e.target.value, 10) : undefined,
              )
            }
            placeholder={t("common.placeholders.example", { value: "1080" })}
            disabled={readOnly}
          />
        </div>
      </div>

      {/* Blocking toggles */}
      <div className="space-y-2">
        <div className="flex items-center gap-x-2">
          <Checkbox
            id="cloak-block-webrtc"
            checked={config.block_webrtc ?? false}
            onCheckedChange={(checked) =>
              onConfigChange("block_webrtc", checked)
            }
            disabled={readOnly}
          />
          <Label htmlFor="cloak-block-webrtc">{t("cloak.blockWebrtc")}</Label>
        </div>
        <div className="flex items-center gap-x-2">
          <Checkbox
            id="cloak-block-webgl"
            checked={config.block_webgl ?? false}
            onCheckedChange={(checked) =>
              onConfigChange("block_webgl", checked)
            }
            disabled={readOnly}
          />
          <Label htmlFor="cloak-block-webgl">{t("cloak.blockWebgl")}</Label>
        </div>
        <div className="flex items-center gap-x-2">
          <Checkbox
            id="cloak-block-images"
            checked={config.block_images ?? false}
            onCheckedChange={(checked) =>
              onConfigChange("block_images", checked)
            }
            disabled={readOnly}
          />
          <Label htmlFor="cloak-block-images">{t("cloak.blockImages")}</Label>
        </div>
      </div>
    </div>
  );
}
