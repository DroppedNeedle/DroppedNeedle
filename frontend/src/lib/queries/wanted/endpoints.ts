import { API } from '$lib/constants';

export const WANTED_ENDPOINTS = {
	list: () => API.requests.wanted(),
	stop: (mbid: string) => API.requests.wantedStop(mbid),
	resume: (mbid: string) => API.requests.wantedResume(mbid),
	seen: (mbid: string) => API.requests.wantedSeen(mbid)
} as const;
