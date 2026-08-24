import { format } from "./shared";
import { api } from "./api";
export const web = () => format(api());
