import { useState } from "react";
import { ArrowUpRight, Check, Copy, MapPin } from "lucide-react";
import type { LocationCard } from "@agent-wechat/shared";
import { googleMapsLocationTarget } from "./location-target.ts";
import "./location-message.css";

export function LocationMessage({ location }: { location: LocationCard }) {
  const [copyState, setCopyState] = useState("");
  const coordinates = `${location.latitude}, ${location.longitude}`;
  const target = googleMapsLocationTarget(location);
  async function copy() {
    try {
      await navigator.clipboard.writeText(coordinates);
      setCopyState("Copied");
    } catch {
      setCopyState("Couldn’t copy");
    }
  }
  const content = (
    <>
      <div className="location-label"><MapPin size={14} aria-hidden="true" />Location</div>
      <div className="location-copy">
        <strong>{location.name || "Shared location"}</strong>
        {location.address && location.address !== location.name && <span>{location.address}</span>}
      </div>
      {target && <span className="location-open-label">Open in Google Maps<ArrowUpRight size={13} aria-hidden="true" /></span>}
    </>
  );
  return (
    <div className="bubble location-card">
      {target ? <a className="location-open" href={target} target="_blank" rel="noopener noreferrer" referrerPolicy="no-referrer">{content}</a> : <div className="location-open">{content}</div>}
      <div className="location-coordinates">
        <span aria-label={`Latitude ${location.latitude}, longitude ${location.longitude}`}>{coordinates}</span>
        <button type="button" onClick={copy} aria-label="Copy coordinates" title="Copy coordinates">
          {copyState === "Copied" ? <Check size={14} aria-hidden="true" /> : <Copy size={14} aria-hidden="true" />}
        </button>
      </div>
      <span className="location-footnote" role="status" title="The coordinate system supplied by WeChat is unspecified.">{copyState || "Coordinates as shared"}</span>
    </div>
  );
}
