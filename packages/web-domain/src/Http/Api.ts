import {
	HttpApi,
	HttpApiEndpoint,
	HttpApiError,
	HttpApiGroup,
	OpenApi,
} from "@effect/platform";
import { Schema } from "effect";
import { HttpAuthMiddleware } from "../Authentication.ts";
import { EXTENSION_HTTP_PREFIX, ExtensionHttpApi } from "../Extension.ts";
import { InternalServerError } from "./Errors.ts";

export const NETWORK_HTTP_PREFIX = "/network";
export const MAX_UPLOAD_HEALTH_PROBE_BYTES = 1024 * 1024;

export const UploadHealthProbeInput = Schema.Struct({
	payload: Schema.String.pipe(
		Schema.maxLength(MAX_UPLOAD_HEALTH_PROBE_BYTES),
	),
});

export const UploadHealthProbeSuccess = Schema.Struct({
	receivedBytes: Schema.Int,
});

export class NetworkHttpApi extends HttpApiGroup.make("network")
	.add(
		HttpApiEndpoint.post("uploadHealth", "/upload-health")
			.middleware(HttpAuthMiddleware)
			.setPayload(UploadHealthProbeInput)
			.addSuccess(UploadHealthProbeSuccess)
			.addError(HttpApiError.BadRequest)
			.addError(InternalServerError),
	)
	.annotateContext(
		OpenApi.annotations({
			title: "Network Health",
			description: "Endpoints used to measure upload throughput.",
		}),
	) {}

export class ApiContract extends HttpApi.make("cap-web-api")
	.add(ExtensionHttpApi.prefix(EXTENSION_HTTP_PREFIX))
	.add(NetworkHttpApi.prefix(NETWORK_HTTP_PREFIX))
	.annotateContext(
		OpenApi.annotations({
			title: "Cap HTTP API",
			description: "Internal API used by Cap Desktop and external services",
		}),
	)
	.prefix("/api") {}
