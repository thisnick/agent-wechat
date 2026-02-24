import { WeChatClient } from "@agent-wechat/shared";
import type { LoginSubscriptionEvent } from "@agent-wechat/shared";

export interface SubscriptionClientOptions {
  url: string;
  token?: string;
  sessionId?: string;
}

// Subscription client interface for WebSocket-based subscriptions
export interface LoginSubscriptionInput {
  timeoutMs?: number;
  newAccount?: boolean;
}

export interface SubscriptionClient {
  status: {
    loginSubscription: {
      subscribe: (
        input: LoginSubscriptionInput,
        callbacks: {
          onData: (event: LoginSubscriptionEvent) => void;
          onError?: (err: Error) => void;
          onComplete?: () => void;
        }
      ) => { unsubscribe: () => void };
    };
  };
}

export interface SubscriptionClientResult {
  client: SubscriptionClient;
  close: () => void;
}

/**
 * Create a WebSocket-capable client for login subscriptions.
 * Wraps WeChatClient.loginSubscribe() from the shared package.
 */
export function createSubscriptionClient(options: SubscriptionClientOptions): SubscriptionClientResult {
  const wechatClient = new WeChatClient({
    baseUrl: options.url,
    token: options.token,
    sessionId: options.sessionId,
  });

  let activeHandle: { close: () => void } | null = null;

  const client: SubscriptionClient = {
    status: {
      loginSubscription: {
        subscribe: (input, callbacks) => {
          const handle = wechatClient.loginSubscribe({
            timeoutMs: input.timeoutMs,
            newAccount: input.newAccount,
            onEvent: callbacks.onData,
            onError: callbacks.onError,
            onClose: callbacks.onComplete,
          });
          activeHandle = handle;

          return {
            unsubscribe: () => {
              handle.close();
              activeHandle = null;
            },
          };
        },
      },
    },
  };

  return {
    client,
    close: () => {
      activeHandle?.close();
      activeHandle = null;
    },
  };
}
