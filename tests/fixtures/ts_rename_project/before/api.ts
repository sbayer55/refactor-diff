import { getUser } from "./users";

export function handle(request: unknown, userId: number) {
  const user = getUser(userId);
  if (user === null) {
    return 404;
  }
  return 200;
}
