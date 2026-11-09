// The accounts the scenes use. A leaf module, so the scene listing can read the scene
// table without loading the SDK (which needs a build).

/**
 * The account every scene signs in with. The simulator names its own contact "Alex
 * Morgan". An alias holds only letters, digits, "-" and "_", and may not equal an ID.
 */
export const ACCOUNT = { id: "main", alias: "alex-morgan" };

/** A second account that is never signed in, for the account list and the login QR. */
export const SPARE = { id: "spare", alias: "spare-phone" };
