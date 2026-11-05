export type PlayerDrawerTab = 'queue' | 'lyrics' | 'eq';

/** The player bar's drawer: whether it is open, and the tab it last showed, kept for the session. */
export const playerDrawer = $state<{ open: boolean; tab: PlayerDrawerTab }>({
	open: false,
	tab: 'queue'
});
