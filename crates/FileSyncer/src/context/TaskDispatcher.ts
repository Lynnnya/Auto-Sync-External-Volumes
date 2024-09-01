import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createContext } from "react";


export class TaskDispatcher {
    private queue: Record<number, Function> = {};
    private listening = false;

    async listen() {
        if (this.listening) return;

        await listen("task_result", (event) => {
            const { id, result } = event.payload as TaskResultPayload<any, any>;
            this.queue[id](result);
            delete this.queue[id];
        });

        this.listening = true;
    }

    submit(msg: Message): Promise<TaskResultOf<typeof msg>> {
        return new Promise((resolve, reject) => {
            invoke("send_message", { msg })
                .then((id) => {
                    this.queue[id as number] = resolve;
                })
                .catch(reject);
        });
    }
};

export const TaskDispatcherContext = createContext<TaskDispatcher>(new TaskDispatcher());