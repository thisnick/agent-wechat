import type { LocationCard } from "@agent-wechat/shared";

// Use shared coordinates unchanged. Map URLs do not establish a coordinate
// system, and WeChat's POI ID is not a Google Place ID.
export function googleMapsCoordinateTarget(location: Pick<LocationCard, "latitude" | "longitude">) {
  if (!Number.isFinite(location.latitude) || !Number.isFinite(location.longitude)
    || Math.abs(location.latitude) > 90 || Math.abs(location.longitude) > 180) return null;
  const url = new URL("https://www.google.com/maps/search/");
  url.searchParams.set("api", "1");
  url.searchParams.set("query", `${location.latitude}, ${location.longitude}`);
  return url.href;
}

export function googleMapsLocationTarget(location: Pick<LocationCard, "name" | "address" | "latitude" | "longitude">) {
  const parts = [location.name, location.address].map(value => value?.trim()).filter((value): value is string => !!value);
  if (!parts.length || parts.some(value => /[\u0000-\u001f\u007f]/.test(value))) return null;
  const place = [...new Set(parts)].join(", ");
  // This supplies search context, not a guaranteed geographic boundary.
  const query = googleMapsCoordinateTarget(location)
    ? `${place} near ${location.latitude}, ${location.longitude}` : place;
  const url = new URL("https://www.google.com/maps/search/");
  url.searchParams.set("api", "1");
  url.searchParams.set("query", query);
  return url.href.length <= 2048 ? url.href : null;
}
