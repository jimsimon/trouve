import { ContextConsumer } from "@lit/context";
import { html, LitElement, nothing } from "lit";

import {
  appStoreContext,
  threadContext,
} from "../contexts/app-contexts.js";
import type { ProtocolTodoItem } from "../services/protocol-client.js";
import { withSignalTracking } from "../state/reactivity.js";
import { buildTodoPlanModel } from "./todo-plan-model.js";
import { fontAwesomeIcon } from "./font-awesome-icon.js";

export class TrouveTodoPlanPanel extends withSignalTracking(LitElement) {
  static override properties = {
    todos: { attribute: false },
  };

  protected override createRenderRoot(): HTMLElement {
    return this;
  }

  todos: readonly ProtocolTodoItem[] | undefined;
  readonly #store = new ContextConsumer(this, {
    context: appStoreContext,
    subscribe: true,
  });
  readonly #threadScope = new ContextConsumer(this, {
    context: threadContext,
    subscribe: true,
  });

  override render() {
    const threadId = this.#threadScope.value?.threadId ?? "";
    const todos = this.todos
      ?? (threadId === "" ? [] : this.#store.value?.threadView(threadId).todos ?? []);
    const plan = buildTodoPlanModel(todos);
    return html`
      <section class="todo-plan-surface" aria-label="Thread todos">
        <header class="todo-plan-header">
          <strong>Thread todos</strong>
          <small>${plan.progressLabel}</small>
        </header>
        ${plan.rows.length === 0
          ? html`<div class="screen-empty todo-plan-empty">
              <strong>No plan items</strong>
              <span>Todos will appear here when the thread publishes a plan.</span>
            </div>`
          : html`<ol class="todo-plan-list">
              ${plan.rows.map((todo) => html`
                <li
                  class=${todo.status}
                  data-todo-id=${todo.id}
                  aria-current=${todo.current ? "step" : nothing}
                >
                  ${fontAwesomeIcon(todo.icon, { className: "todo-plan-icon" })}
                  <span class="todo-plan-content">${todo.content}</span>
                  <span class="visually-hidden">Status: ${todo.statusLabel}</span>
                </li>
              `)}
            </ol>`}
      </section>
    `;
  }
}

customElements.define("trouve-todo-plan-panel", TrouveTodoPlanPanel);

declare global {
  interface HTMLElementTagNameMap {
    "trouve-todo-plan-panel": TrouveTodoPlanPanel;
  }
}
