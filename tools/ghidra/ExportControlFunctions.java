// Emits decompiled C for the stable public Control.dll exports.
// Run only through Ghidra headless analysis; it does not modify the input PE.
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;

public class ExportControlFunctions extends GhidraScript {
    private static final String[] NAMES = {
        "InitializeSettings", "LoadControlSystem", "LoadPhysicalModel",
        "CreateCollManager", "CreateCollObject"
    };
    private static final long[] ADDRESSES = {
        0x10032260L, 0x10032280L, 0x10032580L, 0x100325d0L, 0x10032600L
    };

    @Override
    public void run() throws Exception {
        DecompInterface decompiler = new DecompInterface();
        decompiler.openProgram(currentProgram);
        for (int index = 0; index < NAMES.length; index++) {
            Address address = currentProgram.getAddressFactory()
                .getDefaultAddressSpace().getAddress(ADDRESSES[index]);
            Function function = currentProgram.getFunctionManager().getFunctionAt(address);
            println("\n===== " + NAMES[index] + " =====");
            if (function == null) {
                println("missing");
                continue;
            }
            println(decompiler.decompileFunction(function, 60, monitor)
                .getDecompiledFunction().getC());
        }
        decompiler.dispose();
    }
}
