import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { AppError } from "@/types";

/**
 * Thin wrapper around Tauri's `invoke` that normalises the error shape so
 * callers always get an `AppError` instead of an unknown rejection value.
 */
export async function call<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    throw toAppError(command, error);
  }
}

export function toAppError(command: string, error: unknown): AppError {
  if (typeof error === "string") {
    return { code: command, message: error };
  }

  if (error && typeof error === "object" && "message" in error) {
    const { code, message, path } = error as Partial<AppError>;
    return {
      code: code ?? command,
      message: String(message),
      ...(path ? { path } : {}),
    };
  }

  return { code: command, message: `${command} failed` };
}

/** Subscribe to a backend event stream. Returns the unlisten handle. */
export function subscribe<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  return listen<T>(event, ({ payload }) => handler(payload));
}
