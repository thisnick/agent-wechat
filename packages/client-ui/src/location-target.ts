import type { LocationCard } from "@agent-wechat/shared";

// Search the supplied place text. WeChat's POI ID is not a Google Place ID,
// and its coordinate system is not yet established for coordinate-only pins.
export function googleMapsLocationTarget(location: Pick<LocationCard, "name" | "address">) {
  const parts = [location.name, location.address].map(value => value?.trim()).filter((value): value is string => !!value);
  if (!parts.length || parts.some(value => /[\u0000-\u001f\u007f]/.test(value))) return null;
  const query = [...new Set(parts)].join(", ");
  const url = new URL("https://www.google.com/maps/search/");
  url.searchParams.set("api", "1");
  url.searchParams.set("query", query);
  return url.href.length <= 2048 ? url.href : null;
}
