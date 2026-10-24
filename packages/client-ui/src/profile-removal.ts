import type { Account } from "./session.ts";

export function profileRemovalName(account: Account): string {
  return account.alias || account.id;
}
export function canRemoveProfile(
  account: Account,
  confirmation: string,
): boolean {
  return (
    account.authenticated === false &&
    confirmation === profileRemovalName(account)
  );
}
