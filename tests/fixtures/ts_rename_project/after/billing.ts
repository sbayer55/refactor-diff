import { fetchUser } from "./users";

export function charge(userId: string) {
  const user = fetchUser(userId);
  return user;
}

export function refund(userId: string) {
  return fetchUser(userId);
}
