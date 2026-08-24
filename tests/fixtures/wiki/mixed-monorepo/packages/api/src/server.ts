import { health } from "./health";
export function startServer(): string {
  return health();
}
