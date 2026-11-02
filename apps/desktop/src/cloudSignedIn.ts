import { cloudApi } from "./cloud";
import { useAsync, useEvent } from "./hooks";

/**
 * Whether this app is signed in to Heed Cloud: then Home, Needs you and Search read from the cloud
 * service; otherwise from the local background service. `false` while unknown or unavailable.
 */
export function useCloudSignedIn(): boolean {
  const s = useAsync(() => cloudApi.session().catch(() => null));
  useEvent(cloudApi.onSession, s.reload);
  return s.data?.signed_in === true;
}
