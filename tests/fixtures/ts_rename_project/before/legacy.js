import { getUser } from "./users";

export function oldPath(userId) {
  return getUser(userId);
}
