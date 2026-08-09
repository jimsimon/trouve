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
      <section class="todo-plan-surface" aria-labelledby="todo-plan-title">
        <header class="todo-plan-header">
          <h2 id="todo-plan-title">Todos</h2>
          ${plan.total === 0 ? nothing : html`<small>${plan.progressLabel}</small>`}
        </header>
        ${plan.rows.length === 0
          ? html`<div class="screen-empty todo-plan-empty">
              <span class="todo-plan-empty-icon" aria-hidden="true">
                ${fontAwesomeIcon("list-check")}
              </span>
              <strong>No todos yet</strong>
              <span>When the agent creates a plan, its tasks and progress will appear here.</span>
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
