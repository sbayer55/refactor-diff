import { getUser } from "./users";

export function charge(userId: number) {
  const user = getUser(userId);
  return user;
}

export function refund(userId: number) {
  return getUser(userId);
}
