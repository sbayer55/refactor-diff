import { fetchUser } from "./users";

export function summary(ids: string[]) {
  const users = ids.map((i) => fetchUser(i));
  return { count: users.length, label: "users" };
}
