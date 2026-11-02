import type { CloudBrief } from "../../cloud";
import { CitedProse } from "../../components/CitedProse";

/** The cloud's written morning brief: prose with citation chips and its sources. */
export function BriefProse({ brief }: { brief: CloudBrief }) {
  if (!brief.prose) return null;
  return (
    <div className="brief-prose">
      <CitedProse text={brief.prose} citations={brief.citations} />
    </div>
  );
}
