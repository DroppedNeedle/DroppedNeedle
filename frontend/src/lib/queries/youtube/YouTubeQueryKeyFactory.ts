import { key, SHARED } from '../keys';

// Saved links are shared by everyone on the server.
export const YouTubeQueryKeyFactory = {
	feature: 'youtube',
	links: () => key(SHARED, 'youtube', 'links')
};
