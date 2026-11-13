import { z } from "zod";
import {
  isThrow,
  MAX_CHAT_CODE_POINTS,
  normalizeChatText,
  REACTION_IDS,
} from "../room-interactions.js";
import { opaqueIdSchema } from "./primitives.js";

const chatPayloadSchema = z.object({ kind: z.literal("chat"),
  text: z.string().min(1).max(MAX_CHAT_CODE_POINTS * 2)
    .refine(value => normalizeChatText(value) === value) }).strict();
const reactionPayloadSchema = z.object({ kind: z.literal("reaction"),
  reaction: z.string().min(1).max(32).regex(/^[a-z][a-z0-9-]*$/),
  targetPeerId: opaqueIdSchema.optional() }).strict()
  .refine(payload => !isThrow(payload.reaction) || !!payload.targetPeerId,
  { message: "Throwing a prop requires a participant" });
// Commands and received events share one registered effect set. Identity,
// payload shape and target bounds stay strict.
export const interactionPayloadSchema = z.discriminatedUnion("kind", [
  chatPayloadSchema, reactionPayloadSchema.safeExtend({ reaction: z.enum(REACTION_IDS) }),
]);
export type InteractionPayload = z.infer<typeof interactionPayloadSchema>;
