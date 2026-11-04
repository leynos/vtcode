export interface WebMcpDeployment {
  readonly id: string;
  readonly label: string;
  readonly url: string;
  readonly origin: string;
}

// Hosted deployments are configured by the operator; upstream demos are not Diapason instances.
export const WEBMCP_DEPLOYMENTS: readonly WebMcpDeployment[] = Object.freeze([]);

const LOCAL_WEBMCP_ORIGIN = "http://localhost:5173";

export function browserOrigin(
  location: Pick<Location, "origin"> | null | undefined = globalThis.location,
): string {
  const origin = location?.origin;
  return typeof origin === "string" && origin.length > 0 && origin !== "null"
    ? origin
    : LOCAL_WEBMCP_ORIGIN;
}

export function webmcpDeploymentForOrigin(origin: string): WebMcpDeployment | null {
  return WEBMCP_DEPLOYMENTS.find((deployment) => deployment.origin === origin) ?? null;
}
