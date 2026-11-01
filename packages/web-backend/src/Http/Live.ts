import { Http } from "@cap/web-domain";
import { HttpApiBuilder } from "@effect/platform";
import { Effect, Layer } from "effect";

import { ExtensionHttpLive } from "../Extension/Http.ts";

export const NetworkHttpLive = HttpApiBuilder.group(
	Http.ApiContract,
	"network",
	(handlers) =>
		Effect.succeed(
			handlers.handle("uploadHealth", ({ payload }) =>
				Effect.succeed({
					receivedBytes: payload.payload.length,
				}),
			),
		),
);

export const HttpLive = HttpApiBuilder.api(Http.ApiContract).pipe(
	Layer.provide(Layer.mergeAll(ExtensionHttpLive, NetworkHttpLive)),
);
