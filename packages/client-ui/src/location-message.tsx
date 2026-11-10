import { useState } from "react";
import { ArrowUpRight, Check, Copy, MapPin } from "lucide-react";
import type { LocationCard } from "@agent-wechat/shared";
import { t } from "./i18n.ts";
import { googleMapsCoordinateTarget, googleMapsLocationTarget } from "./location-target.ts";
import "./location-message.css";

export function LocationMessage({ location }: { location: LocationCard }) {
  const [copyState, setCopyState] = useState("");
  const coordinates = `${location.latitude}, ${location.longitude}`;
  const target = googleMapsLocationTarget(location);
  const pinTarget = googleMapsCoordinateTarget(location);
  async function copy() {
    try {
      await navigator.clipboard.writeText(coordinates);
      setCopyState(t("locationMessage.copied"));
    } catch {
      setCopyState(t("locationMessage.couldnTCopy"));
    }
  }
  const content = (
    <>
      <div className="location-label"><MapPin size={14} aria-hidden="true" />{t("locationMessage.location")}</div>
      <div className="location-copy">
        <strong>{location.name || t("locationMessage.sharedLocation")}</strong>
        {location.address && location.address !== location.name && <span>{location.address}</span>}
      </div>
      {target && <span className="location-open-label">{t("locationMessage.searchPlace")}<ArrowUpRight size={13} aria-hidden="true" /></span>}
    </>
  );
  return (
    <div className="bubble location-card">
      {target ? <a className="location-open" href={target} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer">{content}</a> : <div className="location-open">{content}</div>}
      <div className="location-coordinates">
        {pinTarget ? (
          <a className="location-pin-target" href={pinTarget} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer" aria-label={t("locationMessage.viewPinGoogleMapsLatitude", { latitude: location.latitude, longitude: location.longitude })}>
            <span>{coordinates}</span><span className="location-pin-label">{t("locationMessage.viewPin")}<ArrowUpRight size={12} aria-hidden="true" /></span>
          </a>
        ) : <span>{coordinates}</span>}
        <button type="button" onClick={copy} aria-label={t("locationMessage.copyCoordinates")} title={t("locationMessage.copyCoordinates")}>
          {copyState === t("locationMessage.copied") ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
        </button>
      </div>
      <span className="location-footnote" role="status" title={t("locationMessage.coordinateSystemSuppliedWechatUnspecified")}>{copyState || t("locationMessage.coordinatesShared")}</span>
    </div>
  );
}
