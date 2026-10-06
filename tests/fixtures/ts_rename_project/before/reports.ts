import { getUser } from "./users";

export function summary(ids: string[]) {
  const users = ids.map((i) => getUser(i))
  return { count: users.length, label: 'users' }
}
