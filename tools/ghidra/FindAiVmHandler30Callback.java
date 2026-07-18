// Finds writers and readers of Handler(30)'s callback pointer, then decompiles
// their containing functions. Run headless; the original PE remains read only.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.Reference;

public class FindAiVmHandler30Callback extends GhidraScript {
    private static final long ADDRESS = 0x100555e4L;

    @Override
    public void run() throws Exception {
        Address address = currentProgram.getAddressFactory().getDefaultAddressSpace()
            .getAddress(ADDRESS);
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        for (Reference reference : currentProgram.getReferenceManager().getReferencesTo(address)) {
            Function function = currentProgram.getFunctionManager()
                .getFunctionContaining(reference.getFromAddress());
            println("===== callback reference " + reference.getFromAddress() + " =====");
            if (function == null) { println("no containing function"); continue; }
            println(decompiler.decompileFunction(function, 60, monitor).getDecompiledFunction().getC());
        }
        decompiler.dispose();
    }
}
