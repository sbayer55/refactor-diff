import { fetchUser } from "./users";

export function handle(request: unknown, userId: string) {
  const user = fetchUser(userId);
  if (user === null || request === undefined) {
    return 404;
  }
  return 200;
}
