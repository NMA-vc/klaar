import { add } from "./math";
import { User } from "./models";

export function welcome(user: User): string {
  return user.name + ":" + add(user.age, 1);
}
