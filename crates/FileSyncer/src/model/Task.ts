type Message = "InitSpawn" | "ListMounts";

type TaskResultPayload<T, E> = {
    id: number;
    result: TaskResult<T, E>;
}

type TaskResult<T, E> = {
    Ok: T;
} | {
    Err: E;
}

type TaskResultOf<M extends Message> = M extends "InitSpawn" ? TaskResult<null, string> :
    M extends "ListMounts" ? TaskResult<[string, string, string | null][], string> : never;