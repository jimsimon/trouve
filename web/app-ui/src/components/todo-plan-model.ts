import type { ProtocolTodoItem } from "../services/protocol-client.js";
import type { FontAwesomeIconName } from "./font-awesome-icon.js";

export type TodoStatus = ProtocolTodoItem["status"];

export interface TodoPlanRow extends ProtocolTodoItem {
  readonly icon: FontAwesomeIconName;
  readonly statusLabel: "Pending" | "In progress" | "Completed" | "Cancelled";
  readonly current: boolean;
}

export interface TodoPlanModel {
  readonly rows: readonly TodoPlanRow[];
  readonly total: number;
  readonly completed: number;
  readonly pending: number;
  readonly inProgress: number;
  readonly cancelled: number;
  readonly progressLabel: string;
  readonly progressPercent: number;
  readonly current: TodoPlanRow | undefined;
}

const PRESENTATION: Readonly<
  Record<
    TodoStatus,
    Pick<TodoPlanRow, "icon" | "statusLabel">
  >
> = {
  pending: { icon: "circle", statusLabel: "Pending" },
  in_progress: { icon: "play", statusLabel: "In progress" },
  completed: { icon: "check", statusLabel: "Completed" },
  cancelled: { icon: "xmark", statusLabel: "Cancelled" },
};

export const todoStatusIcon = (status: string): FontAwesomeIconName =>
  PRESENTATION[status as TodoStatus]?.icon ?? PRESENTATION.pending.icon;

/** Project the full-replacement protocol snapshot without reordering it. */
export const buildTodoPlanModel = (
  todos: readonly ProtocolTodoItem[],
): TodoPlanModel => {
  const currentIndex = todos.findIndex((todo) => todo.status === "in_progress");
  const rows = todos.map((todo, index) => ({
    ...todo,
    ...PRESENTATION[todo.status],
    current: index === currentIndex,
  }));
  const count = (status: TodoStatus): number =>
    rows.filter((row) => row.status === status).length;
  const completed = count("completed");
  const total = rows.length;
  return {
    rows,
    total,
    completed,
    pending: count("pending"),
    inProgress: count("in_progress"),
    cancelled: count("cancelled"),
    progressLabel: `${completed}/${total} complete`,
    progressPercent: total === 0 ? 0 : Math.round((completed / total) * 100),
    current: rows.find((row) => row.current),
  };
};
