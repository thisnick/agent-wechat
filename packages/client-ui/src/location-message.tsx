import { useState } from "react";
import { ArrowUpRight, Check, Copy, MapPin } from "lucide-react";
import type { LocationCard } from "@agent-wechat/shared";
import { t, useLocale } from "./i18n.ts";
import { googleMapsCoordinateTarget, googleMapsLocationTarget } from "./location-target.ts";
import "./location-message.css";

export function LocationMessage({ location }: { location: LocationCard }) {
  useLocale();
  const [copyState, setCopyState] = useState("");
  const coordinates = `${location.latitude}, ${location.longitude}`;
  const target = googleMapsLocationTarget(location);
  const pinTarget = googleMapsCoordinateTarget(location);
  async function copy() {
    try {
      await navigator.clipboard.writeText(coordinates);
      setCopyState(t("Copied", "已复制"));
    } catch {
      setCopyState(t("Couldn’t copy", "无法复制"));
    }
  }
  const content = (
    <>
      <div className="location-label"><MapPin size={14} aria-hidden="true" />{t("Location", "位置")}</div>
      <div className="location-copy">
        <strong>{location.name || t("Shared location", "分享的位置")}</strong>
        {location.address && location.address !== location.name && <span>{location.address}</span>}
      </div>
      {target && <span className="location-open-label">{t("Search place", "搜索地点")}<ArrowUpRight size={13} aria-hidden="true" /></span>}
    </>
  );
  return (
    <div className="bubble location-card">
      {target ? <a className="location-open" href={target} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer">{content}</a> : <div className="location-open">{content}</div>}
      <div className="location-coordinates">
        {pinTarget ? (
          <a className="location-pin-target" href={pinTarget} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" aria-label={t(`View pin in Google Maps at latitude ${location.latitude}, longitude ${location.longitude}`, `在 Google 地图中查看坐标（纬度 ${location.latitude}，经度 ${location.longitude}）`)}>
            <span>{coordinates}</span><span className="location-pin-label">{t("View pin", "查看标注")}<ArrowUpRight size={12} aria-hidden="true" /></span>
          </a>
        ) : <span>{coordinates}</span>}
        <button type="button" onClick={copy} aria-label={t("Copy coordinates", "复制坐标")} title={t("Copy coordinates", "复制坐标")}>
          {copyState === t("Copied", "已复制") ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
        </button>
      </div>
      <span className="location-footnote" role="status" title={t("The coordinate system supplied by WeChat is unspecified.", "微信提供的坐标系未指定。")}>{copyState || t("Coordinates as shared", "原始分享坐标")}</span>
    </div>
  );
}
