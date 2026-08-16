// based on event types from whisply-rs/exec/src/exec_events.rs

import type { ThreadItem } from "./items";

/** Emitted when a new thread is started as the first event. */
export type ThreadStartedEvent = {
  type: "thread.started";
  /** The identifier of the new thread. Can be used to resume the thread later. */
  thread_id: string;
};

/**
 * Emitted when a turn is started by sending a new prompt to the model.
 * A turn encompasses all events that happen while the agent is processing the prompt.
 */
export type TurnStartedEvent = {
  type: "turn.started";
};

/** Describes the usage of tokens during a turn. */
export type Usage = {
  /** The number of input tokens used during the turn. */
  input_tokens: number;
  /** The number of cached input tokens used during the turn. */
  cached_input_tokens: number;
  /** The number of input tokens written to the prompt cache during the turn. */
  cache_write_input_tokens: number;
  /** The number of output tokens used during the turn. */
  output_tokens: number;
  /** The number of reasoning output tokens used during the turn. */
  reasoning_output_tokens: number;
};

/** One of the account's limit windows. */
export type AccountUsageWindow = {
  /** What the window limits: `usage` or `transcription`. */
  category: string;
  /** The window's period: `five_hour` or `weekly`. */
  window: string;
  /** Cost already charged in this window. */
  settled: number;
  /** Cost held for work that has not settled yet. */
  reserved: number;
  /** The window's ceiling. */
  cap: number;
  /** Settled plus reserved cost as a fraction of the cap. */
  used_fraction: number;
  /** When the window next resets, if a reset is scheduled. */
  resets_at: string | null;
};

/**
 * What a Whisply-metered account has spent, in the cost units it is billed in.
 *
 * Cost already has each model's rate multiplier applied. The multiplier table
 * itself is signed and is identified here by `rate_card_version`; read it with
 * `whisply usage --json` or `whisply models --json`.
 */
export type AccountUsage = {
  /** The account's plan tier. */
  tier: string;
  /** What the amounts below measure, as declared by the metering contract. */
  basis: string;
  /** The currency the amounts are denominated in. */
  currency: string;
  /** The revision of the signed rate card these amounts were priced against. */
  rate_card_version: string;
  /** Every limit window the account has, spend and transcription alike. */
  windows: AccountUsageWindow[];
  /** When the account produced these amounts. */
  generated_at: string;
  /** Whether the amounts are known to be behind the account's live state. */
  stale: boolean;
};

/** Emitted when a turn is completed. Typically right after the assistant's response. */
export type TurnCompletedEvent = {
  type: "turn.completed";
  usage: Usage;
  /**
   * What the account was charged, for an account Whisply meters. Absent for a
   * local or bring-your-own-key run, and for a metered run whose account could
   * not be read.
   */
  account_usage?: AccountUsage;
};

/** Indicates that a turn failed with an error. */
export type TurnFailedEvent = {
  type: "turn.failed";
  error: ThreadError;
};

/** Emitted when a new item is added to the thread. Typically the item is initially "in progress". */
export type ItemStartedEvent = {
  type: "item.started";
  item: ThreadItem;
};

/** Emitted when an item is updated. */
export type ItemUpdatedEvent = {
  type: "item.updated";
  item: ThreadItem;
};

/** Signals that an item has reached a terminal state—either success or failure. */
export type ItemCompletedEvent = {
  type: "item.completed";
  item: ThreadItem;
};

/** Fatal error emitted by the stream. */
export type ThreadError = {
  message: string;
};

/** Represents an unrecoverable error emitted directly by the event stream. */
export type ThreadErrorEvent = {
  type: "error";
  message: string;
};

/** Top-level JSONL events emitted by codex exec. */
export type ThreadEvent =
  | ThreadStartedEvent
  | TurnStartedEvent
  | TurnCompletedEvent
  | TurnFailedEvent
  | ItemStartedEvent
  | ItemUpdatedEvent
  | ItemCompletedEvent
  | ThreadErrorEvent;
