export function areExperimentalFeaturesEnabled(): boolean {
	return process.env.BONE_EXPERIMENTAL === "1";
}
