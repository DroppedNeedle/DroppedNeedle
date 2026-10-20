declare global {
	namespace App {
		interface Error {
			message: string;
			/** Set when the page failed because of the server or the network, not the page. */
			kind?: 'busy' | 'unreachable';
		}
	}
}

export {};
