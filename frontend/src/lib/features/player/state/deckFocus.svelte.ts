// When the deck is on-screen the global player bar hands off to it; reset on unmount.
let deckInView = $state(false);

export const deckFocus = {
	get inView() {
		return deckInView;
	},
	set(value: boolean) {
		deckInView = value;
	}
};
